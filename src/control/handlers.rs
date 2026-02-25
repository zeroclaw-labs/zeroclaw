#![allow(clippy::unused_async, clippy::implicit_hasher)]

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event as SseEvent, KeepAlive},
        IntoResponse, Json, Sse,
    },
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use std::sync::Arc;

use super::store::{Approval, Command};
use crate::gateway::AppState;

fn check_auth(headers: &HeaderMap, state: &AppState) -> Option<(StatusCode, Json<Value>)> {
    if !state.pairing.require_pairing() {
        return None;
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    if !state.pairing.is_authenticated(token) {
        return Some((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        ));
    }
    None
}

pub async fn handle_bots_list(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    match store.list_bots() {
        Ok(bots) => (
            StatusCode::OK,
            Json(json!({"bots": bots, "count": bots.len()})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_bot_detail(
    headers: HeaderMap,
    Path(bot_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    match store.get_bot(&bot_id) {
        Ok(Some(bot)) => {
            let events = store.list_events(Some(&bot_id), 50).unwrap_or_default();
            let commands = store
                .list_commands(Some(&bot_id), None, 50)
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(json!({"bot": bot, "events": events, "commands": commands})),
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Bot not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_bot_delete(
    headers: HeaderMap,
    Path(bot_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let _ = store.audit("admin", "delete_bot", &bot_id, "");
    match store.delete_bot(&bot_id) {
        Ok(true) => {
            if let Some(ref tx) = state.control_events_tx {
                let _ = tx.send(json!({"type": "bot_deleted", "bot_id": bot_id}).to_string());
            }
            (StatusCode::OK, Json(json!({"ok": true})))
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Bot not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let bot_id = body
        .get("bot_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if bot_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "bot_id required"})),
        );
    }
    let bot = super::store::Bot {
        id: bot_id.clone(),
        name: body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&bot_id)
            .to_string(),
        host: body
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        port: u16::try_from(body.get("port").and_then(|v| v.as_u64()).unwrap_or(3000))
            .unwrap_or(3000),
        status: "online".to_string(),
        version: body
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        last_heartbeat: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        channels: body
            .get("channels")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "[]".to_string()),
        provider: body
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        memory_backend: body
            .get("memory_backend")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        uptime_secs: body
            .get("uptime_secs")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        registered_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    if let Err(e) = store.upsert_bot(&bot) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        );
    }
    let _ = store.insert_event(&bot_id, "heartbeat", &body.to_string());

    if let Some(ref tx) = state.control_events_tx {
        let _ = tx.send(json!({"type": "heartbeat", "bot_id": bot_id}).to_string());
    }

    let pending = store.get_pending_commands(&bot_id).unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({"ok": true, "pending_commands": pending})),
    )
}

pub async fn handle_command_create(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let bot_id = body.get("bot_id").and_then(|v| v.as_str()).unwrap_or("");
    let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if bot_id.is_empty() || kind.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "bot_id and kind are required"})),
        );
    }

    let valid_kinds = [
        "reload_config",
        "restart",
        "stop",
        "update_provider",
        "update_channel",
        "update_memory",
        "update_security",
        "run_agent",
        "shell",
    ];
    if !valid_kinds.contains(&kind) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Unknown command kind: {kind}. Valid: {valid_kinds:?}")})),
        );
    }

    let requires_approval = matches!(kind, "restart" | "stop" | "shell" | "update_security");
    let cmd_id = uuid::Uuid::new_v4().to_string();
    let status = if requires_approval {
        "pending_approval"
    } else {
        "approved"
    };

    let cmd = Command {
        id: cmd_id.clone(),
        bot_id: bot_id.to_string(),
        kind: kind.to_string(),
        payload: body
            .get("payload")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string()),
        status: status.to_string(),
        created_by: "admin".to_string(),
        created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        acked_at: None,
        result: None,
        requires_approval,
    };

    if let Err(e) = store.insert_command(&cmd) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        );
    }

    if requires_approval {
        let approval = Approval {
            id: uuid::Uuid::new_v4().to_string(),
            command_id: cmd_id.clone(),
            status: "pending".to_string(),
            reviewer: String::new(),
            reviewed_at: None,
            reason: None,
        };
        let _ = store.insert_approval(&approval);
    }

    let _ = store.audit(
        "admin",
        "create_command",
        &cmd_id,
        &format!("{kind} -> {bot_id}"),
    );

    if let Some(ref tx) = state.control_events_tx {
        let _ = tx.send(
            json!({"type": "command_created", "command_id": cmd_id, "bot_id": bot_id, "kind": kind})
                .to_string(),
        );
    }

    (
        StatusCode::CREATED,
        Json(
            json!({"ok": true, "command_id": cmd_id, "status": status, "requires_approval": requires_approval}),
        ),
    )
}

pub async fn handle_command_ack(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let cmd_id = body
        .get("command_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let status = body
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("acked");
    let result = body.get("result").and_then(|v| v.as_str());

    if cmd_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "command_id required"})),
        );
    }

    let valid_statuses = ["acked", "failed", "running"];
    if !valid_statuses.contains(&status) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid status: {status}")})),
        );
    }

    match store.update_command_status(cmd_id, status, result) {
        Ok(true) => {
            if let Some(ref tx) = state.control_events_tx {
                let _ = tx.send(
                    json!({"type": "command_ack", "command_id": cmd_id, "status": status})
                        .to_string(),
                );
            }
            (StatusCode::OK, Json(json!({"ok": true})))
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Command not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_commands_list(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let bot_id = params.get("bot_id").map(|s| s.as_str());
    let status = params.get("status").map(|s| s.as_str());
    let limit = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100i64);
    match store.list_commands(bot_id, status, limit) {
        Ok(cmds) => (
            StatusCode::OK,
            Json(json!({"commands": cmds, "count": cmds.len()})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_approval_action(
    headers: HeaderMap,
    Path(command_id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if action != "approve" && action != "reject" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "action must be 'approve' or 'reject'"})),
        );
    }
    let reviewer = body
        .get("reviewer")
        .and_then(|v| v.as_str())
        .unwrap_or("admin");
    let reason = body.get("reason").and_then(|v| v.as_str());

    let status = if action == "approve" {
        "approved"
    } else {
        "rejected"
    };

    match store.update_approval(&command_id, status, reviewer, reason) {
        Ok(true) => {
            let _ = store.audit(
                reviewer,
                &format!("{action}_command"),
                &command_id,
                reason.unwrap_or(""),
            );
            if let Some(ref tx) = state.control_events_tx {
                let _ = tx.send(
                    json!({"type": "approval", "command_id": command_id, "action": action})
                        .to_string(),
                );
            }
            (StatusCode::OK, Json(json!({"ok": true, "status": status})))
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Approval not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_approvals_list(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let status = params.get("status").map(|s| s.as_str());
    let limit = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100i64);
    match store.list_approvals(status, limit) {
        Ok(approvals) => (
            StatusCode::OK,
            Json(json!({"approvals": approvals, "count": approvals.len()})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_audit_log(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let limit = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200i64);
    match store.list_audit(limit) {
        Ok(entries) => (
            StatusCode::OK,
            Json(json!({"entries": entries, "count": entries.len()})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_events_list(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err;
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        );
    };
    let bot_id = params.get("bot_id").map(|s| s.as_str());
    let limit = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(100i64);
    match store.list_events(bot_id, limit) {
        Ok(events) => (
            StatusCode::OK,
            Json(json!({"events": events, "count": events.len()})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e}")})),
        ),
    }
}

pub async fn handle_events_stream(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if state.pairing.require_pairing() {
        let header_token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .strip_prefix("Bearer ")
            .unwrap_or("");
        let query_token = params.get("token").map(|s| s.as_str()).unwrap_or("");
        let token = if header_token.is_empty() {
            query_token
        } else {
            header_token
        };
        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Unauthorized"})),
            )
                .into_response();
        }
    }
    let Some(ref tx) = state.control_events_tx else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "SSE not available"})),
        )
            .into_response();
    };
    let rx = tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(data) => Some(Ok::<_, Infallible>(SseEvent::default().data(data))),
        Err(_) => None,
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub async fn handle_control_metrics(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> impl IntoResponse {
    if let Some(err) = check_auth(&headers, &state) {
        return err.into_response();
    }
    let Some(ref store) = state.control_store else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "Control plane not initialized"})),
        )
            .into_response();
    };
    let metrics = super::metrics::ControlMetrics::new(Arc::clone(store));
    (
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        metrics.prometheus_text(),
    )
        .into_response()
}
