//! REST handlers for `/api/slots/*` — dashboard multi-session surface.
//!
//! M1 delivers the data-model surface: create, read, update, delete, and
//! duplicate slots. Messaging (`POST /api/slots/:id/messages`), tool
//! approval, and WS subscription land in M2 per the multi-session
//! dashboard plan.
//!
//! Auth: every handler requires the gateway's bearer token via
//! `require_auth`. When the slot store is unavailable (either
//! `[gateway] session_persistence` is `false`, or SQLite initialization
//! failed) the gateway returns 503 — the dashboard is a stateful
//! feature and cannot operate without persistence.
//!
//! Slot limits: creation is gated by `[gateway.slots]`
//! `soft_limit` / `hard_limit`. Exceeding the soft limit returns 200 with
//! a `Warning` header; exceeding the hard limit returns 429.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{
        IntoResponse, Json, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

use super::AppState;
use super::api::require_auth;
use super::slot::{
    Slot, SlotAgentConfig, SlotCreateRequest, SlotDuplicateRequest, SlotError, SlotListResponse,
    SlotPatchRequest, SlotResponse, SlotState, SlotStore, SlotUpdate,
};
use super::slot_events;

// ── helpers ─────────────────────────────────────────────────────────

/// Return the configured slot store or the 503 response the handler
/// should send when persistence is disabled. Callers match on the
/// outcome rather than propagating a large-Err `Result`.
enum StoreAccess<'a> {
    Available(&'a std::sync::Arc<dyn SlotStore>),
    Unavailable(Response),
}

fn get_store(state: &AppState) -> StoreAccess<'_> {
    match state.slot_store.as_ref() {
        Some(store) => StoreAccess::Available(store),
        None => StoreAccess::Unavailable(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(SlotError::new(
                    "slot_store_unavailable",
                    "Slot persistence is unavailable — set `[gateway] session_persistence = true` and ensure workspace_dir is writable to use /api/slots",
                )),
            )
                .into_response(),
        ),
    }
}

fn err_response(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(SlotError::new(code, message))).into_response()
}

fn io_err(e: std::io::Error) -> Response {
    err_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "slot_store_error",
        format!("slot store failure: {e}"),
    )
}

fn slot_to_response(slot: Slot) -> Response {
    Json(SlotResponse::from(slot)).into_response()
}

fn resolve_title(requested: Option<String>) -> String {
    match requested {
        Some(t) if !t.trim().is_empty() => t,
        _ => "Untitled".to_string(),
    }
}

/// Mint a new gateway-scoped session id.
///
/// The `gw_` prefix is load-bearing: the session REST surface in
/// `api.rs` filters `GET /api/sessions` results via
/// `strip_prefix("gw_")` and synthesizes storage keys as
/// `format!("gw_{id}")` on load/save/delete. Minting without the prefix
/// would make slot-backed sessions invisible to `/api/sessions` and
/// break `/api/sessions/{id}/messages` lookups.
fn mint_session_id() -> String {
    format!("gw_{}", uuid::Uuid::new_v4())
}

/// Outcome of a pre-create slot-limit check. Callers proceed for `Below`
/// and `SoftExceeded`, and return the attached `Response` for
/// `HardExceeded` / `StoreError` without further work.
enum LimitCheck {
    /// The store is below both limits — proceed without ceremony.
    Below,
    /// The store crossed the soft limit after this create; attach a
    /// `Warning` header to the eventual response.
    SoftExceeded { soft_limit: usize, new_count: usize },
    /// Counting or limit enforcement produced a response the caller
    /// should return as-is (hard-limit 429 or slot-store 500).
    Respond(Response),
}

/// Snapshot slot limits from config and current count from the store,
/// then decide whether to allow the pending create. Called by both
/// `POST /api/slots` and `POST /api/slots/:id/duplicate`.
fn check_slot_limit(state: &AppState, store: &std::sync::Arc<dyn SlotStore>) -> LimitCheck {
    let (soft_limit, hard_limit) = {
        let cfg = state.config.lock();
        (
            usize::try_from(cfg.gateway.slots.soft_limit).unwrap_or(usize::MAX),
            usize::try_from(cfg.gateway.slots.hard_limit).unwrap_or(usize::MAX),
        )
    };

    let current_count = match store.count_slots() {
        Ok(n) => n,
        Err(e) => return LimitCheck::Respond(io_err(e)),
    };

    if current_count >= hard_limit {
        let mut response = err_response(
            StatusCode::TOO_MANY_REQUESTS,
            "slot_hard_limit_exceeded",
            format!(
                "Slot hard limit of {hard_limit} reached; delete an existing slot before creating another"
            ),
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return LimitCheck::Respond(response);
    }

    if current_count + 1 > soft_limit {
        LimitCheck::SoftExceeded {
            soft_limit,
            new_count: current_count + 1,
        }
    } else {
        LimitCheck::Below
    }
}

/// Attach the soft-limit `Warning` header (RFC 7234 code 199) when the
/// caller's pre-check reported a soft-limit crossing. No-op otherwise.
fn apply_soft_limit_warning(response: &mut Response, check: &LimitCheck) {
    if let LimitCheck::SoftExceeded {
        soft_limit,
        new_count,
    } = check
    {
        let warning_value = format!(
            "199 - \"Slot soft limit of {soft_limit} exceeded (now {new_count}). Performance may degrade.\""
        );
        if let Ok(header_val) = HeaderValue::from_str(&warning_value) {
            response.headers_mut().insert(header::WARNING, header_val);
        }
    }
}

// ── handlers ────────────────────────────────────────────────────────

/// `GET /api/slots` — list every slot newest-updated first.
pub async fn handle_api_slots_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s,
        StoreAccess::Unavailable(resp) => return resp,
    };

    match store.list_slots() {
        Ok(slots) => Json(SlotListResponse {
            slots: slots.into_iter().map(SlotResponse::from).collect(),
        })
        .into_response(),
        Err(e) => io_err(e),
    }
}

/// `POST /api/slots` — create a slot. Returns 200 (with `Warning` header
/// above soft-limit) or 429 (above hard-limit).
///
/// The request body is optional (`required: false` in the OpenAPI spec);
/// omitting it mints a slot with default title, a fresh `session_id`,
/// and no agent-config overrides.
pub async fn handle_api_slots_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<SlotCreateRequest>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s,
        StoreAccess::Unavailable(resp) => return resp,
    };

    let check = match check_slot_limit(&state, store) {
        LimitCheck::Respond(resp) => return resp,
        c => c,
    };

    let req = body.map(|Json(r)| r).unwrap_or_default();
    let now = Utc::now().timestamp();
    let id = uuid::Uuid::new_v4().to_string();
    let session_id = req.session_id.unwrap_or_else(mint_session_id);
    let title = resolve_title(req.title);

    let mut slot = Slot::new(id, session_id, title, now);
    if let Some(cfg) = req.agent_config {
        slot.agent_config = cfg;
    }
    if let Some(ws) = req.workspace {
        slot.workspace = Some(ws);
    }

    if let Err(e) = store.create_slot(&slot) {
        return io_err(e);
    }

    let mut response = slot_to_response(slot);
    apply_soft_limit_warning(&mut response, &check);
    response
}

/// `GET /api/slots/:id` — load a single slot by id.
pub async fn handle_api_slots_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s,
        StoreAccess::Unavailable(resp) => return resp,
    };

    match store.get_slot(&id) {
        Ok(Some(slot)) => slot_to_response(slot),
        Ok(None) => err_response(
            StatusCode::NOT_FOUND,
            "slot_not_found",
            "Slot does not exist",
        ),
        Err(e) => io_err(e),
    }
}

/// `PATCH /api/slots/:id` — apply a partial update.
pub async fn handle_api_slots_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SlotPatchRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s,
        StoreAccess::Unavailable(resp) => return resp,
    };

    let update: SlotUpdate = req.into();
    match store.update_slot(&id, &update) {
        Ok(Some(slot)) => slot_to_response(slot),
        Ok(None) => err_response(
            StatusCode::NOT_FOUND,
            "slot_not_found",
            "Slot does not exist",
        ),
        Err(e) => io_err(e),
    }
}

/// `DELETE /api/slots/:id` — remove the slot metadata. Does not delete
/// the backing memory session — callers decide that separately via
/// `DELETE /api/sessions/:id`.
///
/// Also drops the slot's warm agent from `SlotRegistry` (M2.5) — a
/// deleted slot shouldn't keep an `Arc<Mutex<Agent>>` alive referring
/// to config that no longer corresponds to a user-visible row.
pub async fn handle_api_slots_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s,
        StoreAccess::Unavailable(resp) => return resp,
    };

    match store.delete_slot(&id) {
        Ok(true) => {
            state.slot_registry.remove(&id);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => err_response(
            StatusCode::NOT_FOUND,
            "slot_not_found",
            "Slot does not exist",
        ),
        Err(e) => io_err(e),
    }
}

/// `POST /api/slots/:id/duplicate` — clone a slot.
///
/// Re-enforces the hard limit (duplicating is effectively creation).
/// `include_history: true` means the duplicate shares the source slot's
/// `session_id`; otherwise a fresh session id is minted.
///
/// The request body is optional; omitting it defaults to
/// `include_history: false` and appends `" (copy)"` to the source title.
pub async fn handle_api_slots_duplicate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<SlotDuplicateRequest>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s,
        StoreAccess::Unavailable(resp) => return resp,
    };

    let source = match store.get_slot(&id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "slot_not_found",
                "Source slot does not exist",
            );
        }
        Err(e) => return io_err(e),
    };

    let check = match check_slot_limit(&state, store) {
        LimitCheck::Respond(resp) => return resp,
        c => c,
    };

    let req = body.map(|Json(r)| r).unwrap_or_default();
    let now = Utc::now().timestamp();
    let new_id = uuid::Uuid::new_v4().to_string();
    let new_session_id = if req.include_history {
        source.session_id.clone()
    } else {
        mint_session_id()
    };
    let new_title = req
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("{} (copy)", source.title));

    let mut new_slot = Slot::new(new_id, new_session_id, new_title, now);
    new_slot.agent_config = source.agent_config;
    new_slot.workspace = source.workspace;

    if let Err(e) = store.create_slot(&new_slot) {
        return io_err(e);
    }

    let mut response = slot_to_response(new_slot);
    apply_soft_limit_warning(&mut response, &check);
    response
}

// ── M2: messaging, stop, approve ─────────────────────────────────────
//
// The SSE messaging handler is intentionally stub-shaped for this M2
// slice: it wires the full request path (auth → store → queue → cancel
// token → state transition → SSE response), but the body of the
// "agent turn" is a minimal acknowledgement-then-done sequence rather
// than a real `Agent::from_config` call.
//
// The warm `SlotRegistry` + shared `Arc<McpRegistry>` refactor specified
// in `multi-session-dashboard.md §4.5` is deferred to M2.5 because it
// requires signature changes across `zeroclaw-runtime`. This stub holds
// the contract boundary steady so the frontend work in M3 can start
// against a stable endpoint while the backend refactor lands in
// parallel.

/// `POST /api/slots/:id/messages` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SlotMessageRequest {
    /// User-authored prompt text for this turn.
    pub content: String,
    /// Optional inline override for this turn only. When omitted, the
    /// slot's stored `agent_config` is used verbatim.
    #[serde(default)]
    pub agent_config: Option<SlotAgentConfig>,
}

/// `POST /api/slots/:id/approve` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SlotApproveRequest {
    /// The `request_id` from the matching `permission_request` event.
    pub request_id: String,
    /// Operator decision: `"approve"`, `"deny"`, or `"always"`.
    pub decision: String,
}

/// Flip a slot's `state` via the store and publish a `slot` event so
/// dashboard subscribers see the transition in real time.
fn publish_slot_state(
    state: &AppState,
    store: &std::sync::Arc<dyn SlotStore>,
    slot_id: &str,
    new_state: SlotState,
) {
    let update = SlotUpdate {
        state: Some(new_state),
        ..Default::default()
    };
    if let Ok(Some(slot)) = store.update_slot(slot_id, &update) {
        let ev = slot_events::slot_updated(&SlotResponse::from(slot));
        let _ = state.event_tx.send(ev);
    }
}

/// `POST /api/slots/:id/messages` — enqueue a user turn and stream the
/// agent's response back as Server-Sent Events.
///
/// M2 pragmatic slice: this handler wires the full request path
/// (auth → store → queue → cancel token → state transition → SSE) and
/// emits a stub acknowledgement + `done` event. The actual Agent spawn
/// is deferred to M2.5; see module docs above.
pub async fn handle_api_slots_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SlotMessageRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s.clone(),
        StoreAccess::Unavailable(resp) => return resp,
    };

    // Confirm slot exists before we claim the queue or flip state.
    let slot = match store.get_slot(&id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "slot_not_found",
                "Slot does not exist",
            );
        }
        Err(e) => return io_err(e),
    };

    // Resolve overrides: turn-local override wins over the slot's
    // stored `agent_config`. These are passed to the SlotRegistry's
    // first-spawn path via `apply_slot_overrides`; subsequent turns
    // reuse the warm agent frozen to its original config snapshot.
    let overrides = req
        .agent_config
        .unwrap_or_else(|| slot.agent_config.clone());
    let base_config = state.config.lock().clone();

    // Get-or-spawn the warm agent before we grab the queue lock. If
    // agent init fails (bad config, missing provider, etc.) we want to
    // return 500 before serializing the client into the queue.
    let slot_entry = match state
        .slot_registry
        .get_or_spawn(&id, &overrides, base_config, state.mcp_registry.clone())
        .await
    {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(slot_id = %id, error = %e, "slot agent init failed");
            return err_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "slot_agent_init_failed",
                format!("Failed to initialise slot agent: {e}"),
            );
        }
    };

    // Acquire the slot-keyed queue slot. If it is busy, return 429 with
    // a Retry-After hint. The guard is held for the duration of the
    // streaming response via the `SlotTurnCleanup` Drop guard below.
    let queue = state.slot_queue.clone();
    let guard = match queue.acquire(&id).await {
        Ok(g) => g,
        Err(e) => {
            let status = match e {
                crate::session_queue::ActorQueueError::QueueFull { .. } => {
                    StatusCode::TOO_MANY_REQUESTS
                }
                crate::session_queue::ActorQueueError::Timeout { .. } => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            };
            let mut resp = err_response(status, "slot_queue_unavailable", format!("{e}"));
            resp.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            return resp;
        }
    };

    // Register a cancel token for this turn so `/stop` can cancel it.
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut tokens = state
            .slot_cancel_tokens
            .lock()
            .expect("slot_cancel_tokens lock poisoned");
        tokens.insert(id.clone(), cancel.clone());
    }

    // Flip state to Running and broadcast.
    publish_slot_state(&state, &store, &id, SlotState::Running);

    // Drop guard: ensures `slot_cancel_tokens` removal + state→Idle
    // transition run even when Axum drops the SSE response future
    // externally (client disconnect mid-stream). See `SlotTurnCleanup`.
    let cleanup = SlotTurnCleanup::new(state.clone(), store.clone(), id.clone(), guard);

    // Spawn the agent turn on a separate task so its `&mut Agent` body
    // is driven independently of the SSE stream polling. We cross this
    // task boundary through:
    //   * `event_tx` — agent → stream, forwards TurnEvents
    //   * `cancel_token` — stream → agent, signals abort
    //   * the return value of the spawn — the final status
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<zeroclaw_api::agent::TurnEvent>(64);
    let agent_handle = slot_entry.agent.clone();
    let pending_approvals = slot_entry.pending_approvals.clone();
    let approval_channel_tx = event_tx.clone();
    let user_content = req.content.clone();
    let cancel_for_task = cancel.clone();
    let turn_task = tokio::spawn(async move {
        let mut agent = agent_handle.lock().await;
        // Register a per-turn WsApprovalChannel against the slot's
        // shared pending-approval map so `POST /api/slots/{id}/approve`
        // can resolve the matching `request_id` while the tool loop is
        // parked. The channel publishes `TurnEvent::ApprovalRequest`
        // into the same mpsc the turn streams through — our SSE
        // mapping at `turn_event_to_chat_delta` then surfaces it as
        // a `permission_request` event tagged with `slot_id`.
        let approval_channel = std::sync::Arc::new(crate::ws_approval::WsApprovalChannel::new(
            approval_channel_tx,
            pending_approvals,
            std::time::Duration::from_secs(120),
        ));
        agent
            .channel_handles()
            .register_channel("ws", approval_channel);
        let result = agent
            .turn_streamed(&user_content, event_tx, Some(cancel_for_task))
            .await;
        agent.channel_handles().unregister_channel("ws");
        result
    });

    // Build the SSE stream. The stream owns the cleanup guard so the
    // slot stays in `Running` state and holds its cancel-token entry
    // until the turn terminates (normal, cancelled, or stream dropped
    // by an abrupt client disconnect).
    let slot_id = id.clone();
    let stream = async_stream::stream! {
        let _cleanup = cleanup; // moved into the stream so Drop fires on stream drop

        loop {
            tokio::select! {
                ev = event_rx.recv() => {
                    match ev {
                        Some(turn_event) => {
                            if let Some(frame) = turn_event_to_chat_delta(&slot_id, &turn_event) {
                                yield Ok::<_, Infallible>(Event::default().data(frame.to_string()));
                            }
                        }
                        None => break, // agent dropped the sender — turn over
                    }
                }
                _ = cancel.cancelled() => {
                    // `/stop` was called. The agent's own cancel-token
                    // handling will terminate its loop; we emit a
                    // transport-level notice and wait for the task to
                    // finish draining.
                    let cancelled = slot_events::chat_delta(&slot_id, "assistant", "[cancelled]", false);
                    yield Ok::<_, Infallible>(Event::default().data(cancelled.to_string()));
                    break;
                }
            }
        }

        // Drain any events the agent produced after break but before
        // the mpsc closed. Without this a trailing `Usage` or final
        // `Chunk` could vanish into the void.
        while let Ok(turn_event) = event_rx.try_recv() {
            if let Some(frame) = turn_event_to_chat_delta(&slot_id, &turn_event) {
                yield Ok::<_, Infallible>(Event::default().data(frame.to_string()));
            }
        }

        // Wait for the turn task to finish so errors surface in
        // tracing logs instead of being lost on task drop.
        if let Err(e) = turn_task.await {
            tracing::warn!(slot_id = %slot_id, error = %e, "slot turn task join failure");
        }

        // Emit the terminal `done` event. Clients use this to
        // finalise their UI state.
        let done_event = slot_events::chat_delta(&slot_id, "assistant", "", true);
        yield Ok::<_, Infallible>(Event::default().data(done_event.to_string()));
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Map a [`TurnEvent`] from the agent's streaming loop to the
/// dashboard's slot-scoped chat-delta frame.
///
/// Returns `None` for events that aren't part of the visible chat
/// stream (e.g. `Usage`). Approval requests are tagged with
/// `slot_id` via `slot_events::permission_request`.
fn turn_event_to_chat_delta(
    slot_id: &str,
    event: &zeroclaw_api::agent::TurnEvent,
) -> Option<serde_json::Value> {
    use zeroclaw_api::agent::TurnEvent as TE;
    match event {
        TE::Chunk { delta } => Some(slot_events::chat_delta(slot_id, "assistant", delta, false)),
        TE::Thinking { delta } => Some(slot_events::chat_delta(slot_id, "thinking", delta, false)),
        TE::ToolCall { id, name, args } => Some(serde_json::json!({
            "type": "chat",
            "slot_id": slot_id,
            "data": {
                "role": "tool_call",
                "id": id,
                "tool": name,
                "arguments": args,
                "done": false,
            },
        })),
        TE::ToolResult { id, name, output } => Some(serde_json::json!({
            "type": "chat",
            "slot_id": slot_id,
            "data": {
                "role": "tool_result",
                "id": id,
                "tool": name,
                "content": output,
                "done": false,
            },
        })),
        TE::ApprovalRequest {
            request_id,
            tool_name,
            arguments_summary,
            timeout_secs,
        } => Some(slot_events::permission_request(
            slot_id,
            request_id,
            tool_name,
            arguments_summary,
            *timeout_secs,
        )),
        // Usage/cost events are not surfaced as chat deltas; they
        // belong on the dashboard's system metrics surface instead.
        TE::Usage { .. } => None,
    }
}

/// RAII cleanup for a slot's messaging turn. Owns the queue guard,
/// `AppState`, store handle, and slot id. On `Drop` it removes the
/// slot's cancel-token entry, flips the slot's state back to `Idle`,
/// and lets the queue guard drop.
///
/// This runs on every stream termination path — normal completion,
/// explicit `/stop` cancellation, and crucially, abrupt client
/// disconnect (Axum drops the SSE response future, which drops the
/// `async_stream` closure, which drops this guard).
struct SlotTurnCleanup {
    state: AppState,
    store: std::sync::Arc<dyn SlotStore>,
    slot_id: String,
    // Option so `Drop` can take it; non-optional in practice.
    _queue_guard: Option<crate::session_queue::QueueGuard>,
}

impl SlotTurnCleanup {
    fn new(
        state: AppState,
        store: std::sync::Arc<dyn SlotStore>,
        slot_id: String,
        queue_guard: crate::session_queue::QueueGuard,
    ) -> Self {
        Self {
            state,
            store,
            slot_id,
            _queue_guard: Some(queue_guard),
        }
    }
}

impl Drop for SlotTurnCleanup {
    fn drop(&mut self) {
        if let Ok(mut tokens) = self.state.slot_cancel_tokens.lock() {
            tokens.remove(&self.slot_id);
        }
        let update = SlotUpdate {
            state: Some(SlotState::Idle),
            ..Default::default()
        };
        if let Ok(Some(slot)) = self.store.update_slot(&self.slot_id, &update) {
            let ev = slot_events::slot_updated(&SlotResponse::from(slot));
            let _ = self.state.event_tx.send(ev);
        }
    }
}

/// `POST /api/slots/:id/stop` — cancel the slot's in-flight turn.
///
/// Returns 200 with `{"status":"aborted"}` when a token was found and
/// cancelled, 200 with `{"status":"no_active_response"}` when the slot
/// exists but no turn is running, 404 when the slot does not exist,
/// 503 when persistence is disabled.
pub async fn handle_api_slots_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s.clone(),
        StoreAccess::Unavailable(resp) => return resp,
    };

    // Confirm the slot exists before we poke the cancel-token registry.
    // Without this check a request for a nonexistent slot would return
    // the generic "no_active_response" shape, which is ambiguous.
    match store.get_slot(&id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "slot_not_found",
                "Slot does not exist",
            );
        }
        Err(e) => return io_err(e),
    }

    let token = state
        .slot_cancel_tokens
        .lock()
        .expect("slot_cancel_tokens lock poisoned")
        .get(&id)
        .cloned();

    if let Some(token) = token {
        token.cancel();
        tracing::info!(slot_id = %id, "slot abort requested");
        Json(serde_json::json!({ "status": "aborted" })).into_response()
    } else {
        Json(serde_json::json!({ "status": "no_active_response" })).into_response()
    }
}

/// `POST /api/slots/:id/approve` — resolve a pending tool approval.
///
/// Looks up the slot's warm-agent entry in `SlotRegistry` and pops the
/// oneshot sender matching `request_id`, delivering the decision to
/// the agent's parked tool loop. Also broadcasts a slot-scoped
/// `approval_response` event so subscribed dashboard clients clear the
/// `WaitingApproval` badge immediately.
///
/// Returns 404 `no_pending_approval` when the slot has no warm agent
/// (never messaged / evicted) or the `request_id` doesn't match.
pub async fn handle_api_slots_approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<SlotApproveRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let store = match get_store(&state) {
        StoreAccess::Available(s) => s.clone(),
        StoreAccess::Unavailable(resp) => return resp,
    };

    // Confirm the slot exists; a request for a nonexistent slot should
    // 404 rather than publishing a stale event or hitting the registry.
    match store.get_slot(&id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return err_response(
                StatusCode::NOT_FOUND,
                "slot_not_found",
                "Slot does not exist",
            );
        }
        Err(e) => return io_err(e),
    }

    // Validate decision up front to return 400 on bad input.
    let normalized = req.decision.to_ascii_lowercase();
    let decision = match normalized.as_str() {
        "approve" => zeroclaw_api::channel::ChannelApprovalResponse::Approve,
        "deny" => zeroclaw_api::channel::ChannelApprovalResponse::Deny,
        "always" => zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
        _ => {
            return err_response(
                StatusCode::BAD_REQUEST,
                "invalid_decision",
                format!(
                    "decision must be one of approve|deny|always (got {:?})",
                    req.decision
                ),
            );
        }
    };

    // Resolve the slot's warm entry and pop the matching oneshot.
    // Slots with no warm agent can't have pending approvals.
    let Some(entry) = state.slot_registry.get(&id) else {
        return err_response(
            StatusCode::NOT_FOUND,
            "no_pending_approval",
            "Slot has no warm agent; no approval is in flight",
        );
    };
    let sender = entry.pending_approvals.lock().remove(&req.request_id);
    let Some(sender) = sender else {
        return err_response(
            StatusCode::NOT_FOUND,
            "no_pending_approval",
            format!(
                "No pending approval for request_id {:?} on slot {:?}",
                req.request_id, id
            ),
        );
    };

    // Deliver the decision. If the receiver has been dropped (turn
    // timed out), the agent's tool loop has already moved on — treat
    // that as idempotent success.
    let _ = sender.send(decision);

    // Broadcast for subscribed sidebars.
    let event = serde_json::json!({
        "type": "approval_response",
        "slot_id": id,
        "data": {
            "request_id": req.request_id,
            "decision": normalized,
        },
    });
    let _ = state.event_tx.send(event);

    Json(serde_json::json!({ "status": "accepted" })).into_response()
}

// ── Handler-level integration tests ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_queue::{SessionActorQueue, SlotActorQueue};
    use axum::body::to_bytes;
    use std::path::Path;
    use std::sync::Arc;
    use zeroclaw_config::schema::Config;
    use zeroclaw_infra::make_slot_store;

    // ── Lightweight mocks for slot tests ───────────────────────────
    //
    // `api_slots` handlers never touch provider or memory, so these
    // stubs only need to satisfy the trait. Signatures match the
    // `api::tests` mocks to keep future drift obvious.

    struct TestSlotProvider;

    #[async_trait::async_trait]
    impl zeroclaw_providers::Provider for TestSlotProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }
    }

    struct TestSlotMemory;

    #[async_trait::async_trait]
    impl zeroclaw_memory::Memory for TestSlotMemory {
        fn name(&self) -> &str {
            "test-slot-memory"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: zeroclaw_memory::MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&zeroclaw_memory::MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    /// Build an `AppState` wired for slot tests: pairing disabled, slot
    /// store backed by a temp SQLite DB, everything else defaulted.
    fn slot_test_state(tmpdir: &Path) -> AppState {
        let workspace_dir = tmpdir.to_path_buf();
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let config = Config {
            workspace_dir: workspace_dir.clone(),
            gateway: zeroclaw_config::schema::GatewayConfig {
                slots: zeroclaw_config::schema::SlotsConfig {
                    soft_limit: 50,
                    hard_limit: 200,
                },
                ..zeroclaw_config::schema::GatewayConfig::default()
            },
            ..Config::default()
        };

        AppState {
            config: Arc::new(parking_lot::Mutex::new(config)),
            provider: Arc::new(TestSlotProvider),
            model: "test-model".into(),
            temperature: 0.0,
            mem: Arc::new(TestSlotMemory),
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
            session_backend: None,
            session_queue: Arc::new(SessionActorQueue::new(8, 30, 600)),
            slot_queue: Arc::new(SlotActorQueue::new(8, 30, 600)),
            slot_store: Some(make_slot_store(&workspace_dir).unwrap()),
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

    async fn body_to_json(response: Response) -> serde_json::Value {
        let body = response.into_body();
        let bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    fn create_body(title: &str) -> Option<Json<SlotCreateRequest>> {
        Some(Json(SlotCreateRequest {
            title: Some(title.into()),
            session_id: None,
            agent_config: None,
            workspace: None,
        }))
    }

    #[tokio::test]
    async fn slots_rest_create_list_get_patch_delete_flow() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        // 1) CREATE
        let create_response = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("First slot"),
        )
        .await;
        assert_eq!(create_response.status(), StatusCode::OK);
        let created = body_to_json(create_response).await;
        let slot_id = created["id"].as_str().expect("id present").to_string();
        assert_eq!(created["title"], "First slot");
        assert_eq!(created["state"], "idle");
        assert!(created["session_id"].as_str().unwrap().starts_with("gw_"));

        // 2) LIST
        let list_response = handle_api_slots_list(State(state.clone()), HeaderMap::new()).await;
        assert_eq!(list_response.status(), StatusCode::OK);
        let listed = body_to_json(list_response).await;
        let slots = listed["slots"].as_array().expect("slots array");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["id"], slot_id);

        // 3) GET
        let get_response = handle_api_slots_get(
            State(state.clone()),
            HeaderMap::new(),
            Path(slot_id.clone()),
        )
        .await;
        assert_eq!(get_response.status(), StatusCode::OK);

        // 4) PATCH
        let patch_response = handle_api_slots_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(slot_id.clone()),
            Json(SlotPatchRequest {
                title: Some("Renamed".into()),
                ..SlotPatchRequest::default()
            }),
        )
        .await;
        assert_eq!(patch_response.status(), StatusCode::OK);
        let patched = body_to_json(patch_response).await;
        assert_eq!(patched["title"], "Renamed");

        // 5) DELETE
        let delete_response = handle_api_slots_delete(
            State(state.clone()),
            HeaderMap::new(),
            Path(slot_id.clone()),
        )
        .await;
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        // 6) GET after delete → 404
        let after_delete =
            handle_api_slots_get(State(state.clone()), HeaderMap::new(), Path(slot_id)).await;
        assert_eq!(after_delete.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn slots_rest_soft_limit_adds_warning_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        // Lower the soft limit to something testable without actually
        // creating 50+ slots.
        {
            let mut cfg = state.config.lock();
            cfg.gateway.slots.soft_limit = 2;
            cfg.gateway.slots.hard_limit = 10;
        }

        for i in 0..2 {
            let resp = handle_api_slots_create(
                State(state.clone()),
                HeaderMap::new(),
                create_body(&format!("Below soft {i}")),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(
                resp.headers().get(header::WARNING).is_none(),
                "no Warning header below soft limit"
            );
        }

        // Third create crosses the soft limit (count = 3 > 2 = soft limit)
        let third = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Soft crossed"),
        )
        .await;
        assert_eq!(third.status(), StatusCode::OK);
        let warn = third
            .headers()
            .get(header::WARNING)
            .expect("Warning header present above soft limit");
        let warn_text = warn.to_str().unwrap();
        assert!(
            warn_text.contains("Slot soft limit"),
            "Warning header should mention soft limit: {warn_text}"
        );
    }

    #[tokio::test]
    async fn slots_rest_hard_limit_returns_429_with_retry_after() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        {
            let mut cfg = state.config.lock();
            cfg.gateway.slots.soft_limit = 1;
            cfg.gateway.slots.hard_limit = 2;
        }

        // Exhaust the hard limit.
        for i in 0..2 {
            let resp = handle_api_slots_create(
                State(state.clone()),
                HeaderMap::new(),
                create_body(&format!("{i}")),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }

        // 3rd create: 429 with Retry-After.
        let rejected = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Third should fail"),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            rejected.headers().get(header::RETRY_AFTER).is_some(),
            "Retry-After header required on hard-limit rejection"
        );
        let json = body_to_json(rejected).await;
        assert_eq!(json["code"], "slot_hard_limit_exceeded");
    }

    #[tokio::test]
    async fn slots_rest_patch_404_for_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let response = handle_api_slots_patch(
            State(state),
            HeaderMap::new(),
            Path("no-such".into()),
            Json(SlotPatchRequest {
                title: Some("hi".into()),
                ..SlotPatchRequest::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = body_to_json(response).await;
        assert_eq!(json["code"], "slot_not_found");
    }

    #[tokio::test]
    async fn slots_rest_duplicate_without_history_mints_new_session_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        // Create source slot.
        let source = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Source"),
        )
        .await;
        let source_json = body_to_json(source).await;
        let source_id = source_json["id"].as_str().unwrap().to_string();
        let source_session = source_json["session_id"].as_str().unwrap().to_string();

        // Duplicate without history.
        let dup = handle_api_slots_duplicate(
            State(state.clone()),
            HeaderMap::new(),
            Path(source_id.clone()),
            Some(Json(SlotDuplicateRequest {
                title: None,
                include_history: false,
            })),
        )
        .await;
        assert_eq!(dup.status(), StatusCode::OK);
        let dup_json = body_to_json(dup).await;
        assert_ne!(
            dup_json["session_id"], source_session,
            "duplicate without history must mint a fresh session id"
        );
        assert_ne!(dup_json["id"], source_id, "duplicate id must be new");
        assert_eq!(dup_json["title"], "Source (copy)");
    }

    #[tokio::test]
    async fn slots_rest_duplicate_with_history_shares_session_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let source = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Shared"),
        )
        .await;
        let source_json = body_to_json(source).await;
        let source_id = source_json["id"].as_str().unwrap().to_string();
        let source_session = source_json["session_id"].as_str().unwrap().to_string();

        let dup = handle_api_slots_duplicate(
            State(state),
            HeaderMap::new(),
            Path(source_id),
            Some(Json(SlotDuplicateRequest {
                title: Some("Copy with history".into()),
                include_history: true,
            })),
        )
        .await;
        assert_eq!(dup.status(), StatusCode::OK);
        let dup_json = body_to_json(dup).await;
        assert_eq!(
            dup_json["session_id"], source_session,
            "duplicate with history must share the source session id"
        );
        assert_eq!(dup_json["title"], "Copy with history");
    }

    #[tokio::test]
    async fn slots_rest_returns_503_when_store_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut state = slot_test_state(tmp.path());
        state.slot_store = None; // simulate disabled persistence
        let response = handle_api_slots_list(State(state), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── M2: messaging, stop, approve ────────────────────────────────

    #[tokio::test]
    async fn slots_rest_messages_returns_404_for_missing_slot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let resp = handle_api_slots_messages(
            State(state),
            HeaderMap::new(),
            Path("nope".into()),
            Json(SlotMessageRequest {
                content: "hi".into(),
                agent_config: None,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_to_json(resp).await;
        assert_eq!(json["code"], "slot_not_found");
    }

    #[tokio::test]
    async fn slots_rest_messages_hits_full_pipeline_up_to_agent_spawn() {
        // M2.5: the messaging handler now drives a real agent via
        // `SlotRegistry::get_or_spawn`, which calls
        // `Agent::from_config_with_shared_mcp_backchannel`. The
        // `slot_test_state` fixture has no provider configured in
        // `Config`, so agent init fails — which is the success signal
        // for this test: it proves the full request path (auth →
        // store → slot-exists → override apply → registry
        // get-or-spawn) wires up, and isolates the failure to the
        // agent-init boundary.
        //
        // A full-loop E2E with a stub provider is a follow-up (needs
        // an injectable Provider factory on `AppState`).
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let created =
            handle_api_slots_create(State(state.clone()), HeaderMap::new(), create_body("M2-1"))
                .await;
        let slot_id = body_to_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = handle_api_slots_messages(
            State(state.clone()),
            HeaderMap::new(),
            Path(slot_id.clone()),
            Json(SlotMessageRequest {
                content: "hello".into(),
                agent_config: None,
            }),
        )
        .await;

        // Full happy-path would be 200 OK + SSE. With no working
        // provider in the test config, agent spawn returns 500.
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_to_json(resp).await;
        assert_eq!(json["code"], "slot_agent_init_failed");
    }

    #[tokio::test]
    async fn slots_rest_stop_no_active_turn_returns_no_active_response() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let created = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Idle slot"),
        )
        .await;
        let slot_id = body_to_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = handle_api_slots_stop(State(state), HeaderMap::new(), Path(slot_id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_to_json(resp).await;
        assert_eq!(json["status"], "no_active_response");
    }

    #[tokio::test]
    async fn slots_rest_stop_returns_404_for_missing_slot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let resp = handle_api_slots_stop(State(state), HeaderMap::new(), Path("nope".into())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn slots_rest_stop_cancels_in_flight_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let created = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Cancel me"),
        )
        .await;
        let slot_id = body_to_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Inject a cancel token as if a turn were in flight. This sidesteps
        // the need to race against the SSE stream's own setup/teardown.
        let token = tokio_util::sync::CancellationToken::new();
        state
            .slot_cancel_tokens
            .lock()
            .unwrap()
            .insert(slot_id.clone(), token.clone());

        assert!(!token.is_cancelled());
        let resp = handle_api_slots_stop(State(state), HeaderMap::new(), Path(slot_id)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_to_json(resp).await;
        assert_eq!(json["status"], "aborted");
        assert!(token.is_cancelled(), "stop handler must cancel the token");
    }

    #[tokio::test]
    async fn slots_rest_approve_validates_decision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let created =
            handle_api_slots_create(State(state.clone()), HeaderMap::new(), create_body("A1"))
                .await;
        let slot_id = body_to_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let bad = handle_api_slots_approve(
            State(state.clone()),
            HeaderMap::new(),
            Path(slot_id.clone()),
            Json(SlotApproveRequest {
                request_id: "r".into(),
                decision: "maybe".into(),
            }),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        let bad_json = body_to_json(bad).await;
        assert_eq!(bad_json["code"], "invalid_decision");
    }

    #[tokio::test]
    async fn slots_rest_approve_returns_no_pending_when_no_warm_agent() {
        // M2.5: when the slot has never been messaged (no warm
        // `SlotEntry` in `SlotRegistry`), `/approve` can't possibly
        // have a pending oneshot to resolve. The handler returns
        // `404 no_pending_approval` to make that state explicit.
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let created =
            handle_api_slots_create(State(state.clone()), HeaderMap::new(), create_body("A2"))
                .await;
        let slot_id = body_to_json(created).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = handle_api_slots_approve(
            State(state),
            HeaderMap::new(),
            Path(slot_id),
            Json(SlotApproveRequest {
                request_id: "req-1".into(),
                decision: "approve".into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_to_json(resp).await;
        assert_eq!(json["code"], "no_pending_approval");
    }

    #[tokio::test]
    async fn slots_rest_approve_returns_404_for_missing_slot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());
        let resp = handle_api_slots_approve(
            State(state),
            HeaderMap::new(),
            Path("nope".into()),
            Json(SlotApproveRequest {
                request_id: "r".into(),
                decision: "approve".into(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Optional-body + workspace-clear coverage (Copilot review #1/#2/#5) ──

    #[tokio::test]
    async fn slots_rest_create_accepts_missing_body() {
        // OpenAPI declares the body optional — a `POST /api/slots` with no
        // JSON body must yield a default slot instead of 400.
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let resp = handle_api_slots_create(State(state), HeaderMap::new(), None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_to_json(resp).await;
        assert_eq!(json["title"], "Untitled");
        assert_eq!(json["state"], "idle");
        assert!(
            json["session_id"].as_str().unwrap().starts_with("gw_"),
            "default-bodied slot must still mint a gw_-prefixed session id"
        );
    }

    #[tokio::test]
    async fn slots_rest_duplicate_accepts_missing_body() {
        // `POST /api/slots/:id/duplicate` body is also optional — omitting
        // it must default to `include_history=false` and "(copy)" suffix.
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let source = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            create_body("Parent"),
        )
        .await;
        let source_json = body_to_json(source).await;
        let source_id = source_json["id"].as_str().unwrap().to_string();
        let source_session = source_json["session_id"].as_str().unwrap().to_string();

        let dup = handle_api_slots_duplicate(
            State(state),
            HeaderMap::new(),
            Path(source_id.clone()),
            None,
        )
        .await;
        assert_eq!(dup.status(), StatusCode::OK);
        let dup_json = body_to_json(dup).await;
        assert_eq!(dup_json["title"], "Parent (copy)");
        assert_ne!(
            dup_json["session_id"], source_session,
            "default duplicate must mint a fresh session id"
        );
        assert_ne!(dup_json["id"], source_id);
    }

    #[tokio::test]
    async fn slots_rest_patch_clear_workspace_nulls_the_label() {
        // Create a slot with a workspace label, then PATCH with
        // `clear_workspace: true` and verify the label is null on GET.
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let create_resp = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            Some(Json(SlotCreateRequest {
                title: Some("Labeled".into()),
                workspace: Some("home-lab".into()),
                ..SlotCreateRequest::default()
            })),
        )
        .await;
        let slot_id = body_to_json(create_resp).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let patch_resp = handle_api_slots_patch(
            State(state.clone()),
            HeaderMap::new(),
            Path(slot_id.clone()),
            Json(SlotPatchRequest {
                clear_workspace: true,
                ..SlotPatchRequest::default()
            }),
        )
        .await;
        assert_eq!(patch_resp.status(), StatusCode::OK);
        let patched = body_to_json(patch_resp).await;
        assert!(
            patched["workspace"].is_null(),
            "clear_workspace:true must null the label, got {}",
            patched["workspace"]
        );

        // Round-trip through GET to confirm persistence.
        let get_resp = handle_api_slots_get(State(state), HeaderMap::new(), Path(slot_id)).await;
        let got = body_to_json(get_resp).await;
        assert!(got["workspace"].is_null());
    }

    #[tokio::test]
    async fn slots_rest_patch_clear_workspace_wins_over_workspace_field() {
        // When a caller sends both `clear_workspace: true` and
        // `workspace: "something"`, the clear signal wins.
        let tmp = tempfile::TempDir::new().unwrap();
        let state = slot_test_state(tmp.path());

        let create_resp = handle_api_slots_create(
            State(state.clone()),
            HeaderMap::new(),
            Some(Json(SlotCreateRequest {
                title: Some("Both".into()),
                workspace: Some("original".into()),
                ..SlotCreateRequest::default()
            })),
        )
        .await;
        let slot_id = body_to_json(create_resp).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let patch_resp = handle_api_slots_patch(
            State(state),
            HeaderMap::new(),
            Path(slot_id),
            Json(SlotPatchRequest {
                workspace: Some("ignored".into()),
                clear_workspace: true,
                ..SlotPatchRequest::default()
            }),
        )
        .await;
        let patched = body_to_json(patch_resp).await;
        assert!(patched["workspace"].is_null());
    }
}
