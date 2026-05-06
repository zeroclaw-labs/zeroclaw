//! `/api/system/*` — version check + self-update endpoints.
//!
//! Backed by [`zeroclaw_updater`], which is the same pipeline `zeroclaw
//! update` runs on the CLI. The gateway adds a small bit of state to track
//! whether an update is currently running (so we can return 409 instead of
//! kicking off a second one) and an SSE stream for live phase progress.

use super::AppState;
use super::api::require_auth;
use anyhow::Result;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
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
use zeroclaw_updater::{self as updater, UpdateEvent, UpdatePhase};

/// Capacity of the recent-events ring buffer surfaced by `/status`.
const RECENT_EVENTS_CAPACITY: usize = 64;

/// Capacity of the broadcast channel for live SSE consumers.
const SSE_BROADCAST_CAPACITY: usize = 128;

/// How long a `GET /api/system/version` response can be served from cache
/// before re-querying GitHub. Caps the gateway at 1 GitHub call / 30s
/// regardless of how many dashboard tabs are polling — important because
/// the request is unauthenticated against the GitHub API (60/hr/IP limit).
const VERSION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Tracks an in-progress (or just-completed) update run.
///
/// Clones of `UpdateState` (held inside `AppState`) all observe the same
/// snapshot, so handlers can inspect/mutate the run from any axum task.
pub struct UpdateState {
    inner: Mutex<UpdateStateInner>,
    events_tx: broadcast::Sender<UpdateEvent>,
    /// Memoised GitHub releases response. The cache is populated on first
    /// successful `/api/system/version` and re-fetched after `VERSION_CACHE_TTL`.
    /// Uses `tokio::sync::Mutex` so the GitHub call (held under the lock for
    /// single-flight) doesn't block other handlers — only concurrent
    /// version requests serialize on it.
    version_cache: tokio::sync::Mutex<Option<CachedVersion>>,
}

struct CachedVersion {
    info: updater::UpdateInfo,
    fetched_at: std::time::Instant,
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
            version_cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Return cached `UpdateInfo` if the cache is still fresh, otherwise
    /// re-fetch from GitHub through the updater crate and replace the
    /// cache. Caller-side `target_version` bypasses the cache (the cache
    /// only ever holds the latest-tag response).
    async fn cached_version(&self, target_version: Option<&str>) -> Result<updater::UpdateInfo> {
        if target_version.is_some() {
            return updater::check(target_version).await;
        }
        let mut guard = self.version_cache.lock().await;
        if let Some(cached) = guard.as_ref()
            && cached.fetched_at.elapsed() < VERSION_CACHE_TTL
        {
            return Ok(cached.info.clone());
        }
        let info = updater::check(None).await?;
        *guard = Some(CachedVersion {
            info: info.clone(),
            fetched_at: std::time::Instant::now(),
        });
        Ok(info)
    }

    /// Start tracking a new run. Returns `false` when one is already active.
    ///
    /// The previous run's `recent` log buffer is preserved until a new run
    /// emits its first event — this lets an operator who polls `/status`
    /// after a completed run still see what happened, even if a fresh run
    /// started in the meantime.
    pub(crate) fn try_start(&self, task_id: &str) -> bool {
        let mut g = self.inner.lock();
        if let Some(run) = &g.current
            && run.success.is_none()
        {
            return false;
        }
        g.current = Some(UpdateRun {
            task_id: task_id.to_string(),
            phase: UpdatePhase::Preflight,
            started_at: chrono::Utc::now(),
            success: None,
        });
        // Note: `g.recent` is *not* cleared here. `record_event` truncates
        // older entries naturally as the new run fills the ring buffer; that
        // way the prior run's terminal events stay visible until they age
        // out, instead of disappearing the moment a new run is requested.
        true
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
///
/// Cached for `VERSION_CACHE_TTL` so dashboard polling (60s/tab × N tabs ×
/// many deployments) doesn't burn through GitHub's unauthenticated
/// 60-call/hr rate limit.
pub async fn handle_get_version(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match state.update_state.cached_version(None).await {
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
    /// Used to re-install after a corrupted swap or to pin to a specific
    /// `version` tag.
    #[serde(default)]
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
    // UUID v4 over a millisecond timestamp — collision-free even if
    // single-flight is later relaxed, and the format stays opaque to clients.
    let task_id = format!("upd_{}", uuid::Uuid::new_v4());

    if !state.update_state.try_start(&task_id) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "an update is already in progress",
            })),
        )
            .into_response();
    }

    spawn_update_task(
        state.update_state.clone(),
        task_id,
        body.version,
        body.force,
    );

    // Single-flight makes the task id non-load-bearing for the client
    // (`/status` and `/stream` are global, not per-task), so the response
    // intentionally has no body — just `202 Accepted` to acknowledge the
    // run started.
    StatusCode::ACCEPTED.into_response()
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
    // Plain-text 401 (the SSE shape on success is `text/event-stream`, so
    // the JSON envelope from `require_auth` would be misleading on error).
    // Auth predicate itself is the same as the JSON handlers — we just
    // render the failure differently.
    if !super::api::is_authenticated_request(&state, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            "Unauthorized — provide Authorization: Bearer <token>",
        )
            .into_response();
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

fn spawn_update_task(
    state: Arc<UpdateState>,
    task_id: String,
    target_version: Option<String>,
    force: bool,
) {
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

        let _ = updater::run_with_progress(
            target_version.as_deref(),
            &task_id,
            tx,
            updater::RunOptions { force },
        )
        .await;

        let _ = forwarder.await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(task_id: &str, phase: UpdatePhase) -> UpdateEvent {
        UpdateEvent {
            task_id: task_id.into(),
            phase,
            level: updater::UpdateLevel::Info,
            message: format!("{phase:?}"),
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn try_start_returns_false_when_run_active() {
        let state = UpdateState::new();
        assert!(state.try_start("a"));
        assert!(
            !state.try_start("b"),
            "second concurrent start should be rejected"
        );
    }

    #[tokio::test]
    async fn try_start_succeeds_after_terminal_event() {
        let state = UpdateState::new();
        assert!(state.try_start("a"));
        state.record_event(&make_event("a", UpdatePhase::Done));
        assert!(
            state.try_start("b"),
            "new run should be allowed once previous is in a terminal state"
        );
    }

    #[tokio::test]
    async fn try_start_preserves_previous_recent_buffer() {
        // Reviewer ask: don't drop the prior run's history just because a
        // new run kicks off. Operators polling /status after a finished run
        // should still see what happened.
        let state = UpdateState::new();
        assert!(state.try_start("a"));
        state.record_event(&make_event("a", UpdatePhase::Download));
        state.record_event(&make_event("a", UpdatePhase::Done));
        assert!(state.try_start("b"));
        let snap = state.snapshot();
        // Both prior events are still in the ring buffer at this point;
        // they age out naturally as run "b" emits its own events.
        assert_eq!(snap.recent.len(), 2);
        assert!(snap.recent.iter().all(|e| e.task_id == "a"));
    }

    #[tokio::test]
    async fn snapshot_status_succeeded_after_done() {
        let state = UpdateState::new();
        state.try_start("a");
        state.record_event(&make_event("a", UpdatePhase::Done));
        let snap = state.snapshot();
        assert_eq!(snap.run.as_ref().and_then(|r| r.success), Some(true));
    }

    #[tokio::test]
    async fn snapshot_status_failed_after_rolled_back() {
        let state = UpdateState::new();
        state.try_start("a");
        state.record_event(&make_event("a", UpdatePhase::RolledBack));
        let snap = state.snapshot();
        assert_eq!(snap.run.as_ref().and_then(|r| r.success), Some(false));
    }

    #[tokio::test]
    async fn snapshot_status_failed_after_failed_phase() {
        let state = UpdateState::new();
        state.try_start("a");
        state.record_event(&make_event("a", UpdatePhase::Failed));
        let snap = state.snapshot();
        assert_eq!(snap.run.as_ref().and_then(|r| r.success), Some(false));
    }

    #[tokio::test]
    async fn snapshot_includes_recent_events() {
        let state = UpdateState::new();
        state.try_start("a");
        for _ in 0..3 {
            state.record_event(&make_event("a", UpdatePhase::Download));
        }
        let snap = state.snapshot();
        assert_eq!(snap.recent.len(), 3);
        assert!(snap.run.is_some());
    }

    #[tokio::test]
    async fn ring_buffer_caps_at_capacity() {
        // Push more than RECENT_EVENTS_CAPACITY events; verify we never
        // exceed the cap and that the oldest events are evicted (the first
        // event should be gone).
        let state = UpdateState::new();
        state.try_start("a");
        let push_count = RECENT_EVENTS_CAPACITY + 5;
        for i in 0..push_count {
            state.record_event(&UpdateEvent {
                task_id: "a".into(),
                phase: UpdatePhase::Download,
                level: updater::UpdateLevel::Info,
                message: format!("event-{i}"),
                timestamp: chrono::Utc::now(),
            });
        }
        let snap = state.snapshot();
        assert_eq!(snap.recent.len(), RECENT_EVENTS_CAPACITY);
        // First event ("event-0") must have been evicted; oldest remaining
        // should be "event-5" (push_count - capacity = 5).
        assert_eq!(snap.recent.first().unwrap().message, "event-5");
        assert_eq!(
            snap.recent.last().unwrap().message,
            format!("event-{}", push_count - 1)
        );
    }
}
