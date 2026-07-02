//! SOP authoring surface for the web node editor.
//!
//! HTTP twin of the daemon's `sops/*` RPC methods, backed by the same
//! `zeroclaw_runtime::sop` authoring core (load/save/delete, graph
//! projection, wire edits, trigger registry). All routes require gateway
//! auth. Draft endpoints (`wire-draft`, `graph-draft`) are pure: they
//! transform the submitted SOP and never touch disk.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::AppState;
use super::api::require_auth;

fn sops_dir_and_mode(
    state: &AppState,
) -> (std::path::PathBuf, zeroclaw_runtime::sop::SopExecutionMode) {
    let config = state.config.read();
    let workspace = config.shared_workspace_dir();
    let dir = zeroclaw_runtime::sop::resolve_sops_dir(&workspace, config.sop.sops_dir.as_deref());
    let mode = zeroclaw_runtime::sop::parse_execution_mode(&config.sop.default_execution_mode);
    (dir, mode)
}

pub async fn handle_sops_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    let sops = zeroclaw_runtime::sop::load_sops_from_directory(&dir, mode);
    Json(serde_json::json!({ "sops": sops })).into_response()
}

pub async fn handle_sop_trigger_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let registry = {
        let config = state.config.read();
        zeroclaw_runtime::sop::registry_from_config(&config)
    };
    Json(registry).into_response()
}

pub async fn handle_sop_graph(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => {
            let graph = zeroclaw_runtime::sop::SopGraph::from_sop(&sop);
            Json(graph).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
        )
            .into_response(),
    }
}

pub async fn handle_sop_run_overlay(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((name, run_id)): Path<(String, String)>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    let sop = match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => sop,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
            )
                .into_response();
        }
    };
    let Some(engine) = state.sop_engine.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "SOP subsystem not enabled" })),
        )
            .into_response();
    };
    match zeroclaw_runtime::sop::run_overlay_for(&sop, engine, &run_id) {
        Ok(overlay) => Json(overlay).into_response(),
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

pub async fn handle_sop_full(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::load_sop_by_name(&dir, &name, mode) {
        Ok(sop) => Json(sop).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("SOP '{name}': {e}") })),
        )
            .into_response(),
    }
}

pub async fn handle_sop_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(sop): Json<zeroclaw_runtime::sop::Sop>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, _mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::create_sop(&dir, &sop) {
        Ok(()) => Json(serde_json::json!({ "created": sop.name })).into_response(),
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(serde_json::json!({ "error": msg }))).into_response()
        }
    }
}

pub async fn handle_sop_save(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(_name): Path<String>,
    Json(sop): Json<zeroclaw_runtime::sop::Sop>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, _mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::save_sop(&dir, &sop) {
        Ok(()) => Json(serde_json::json!({ "saved": sop.name })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn handle_sop_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, _mode) = sops_dir_and_mode(&state);
    match zeroclaw_runtime::sop::delete_sop(&dir, &name) {
        Ok(()) => Json(serde_json::json!({ "deleted": name })).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Body for `wire-draft`: a full draft SOP plus one edit to apply.
#[derive(serde::Deserialize)]
pub struct WireDraftRequest {
    pub sop: zeroclaw_runtime::sop::Sop,
    pub edit: zeroclaw_runtime::sop::WireEdit,
}

pub async fn handle_sop_wire_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WireDraftRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let mut sop = req.sop;
    if let Err(e) = zeroclaw_runtime::sop::apply_wire(&mut sop, &req.edit) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let graph = zeroclaw_runtime::sop::SopGraph::from_sop(&sop);
    Json(serde_json::json!({ "sop": sop, "graph": graph })).into_response()
}

/// Body for `graph-draft`: a full draft SOP to project without saving.
#[derive(serde::Deserialize)]
pub struct GraphDraftRequest {
    pub sop: zeroclaw_runtime::sop::Sop,
}

pub async fn handle_sop_graph_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GraphDraftRequest>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let graph = zeroclaw_runtime::sop::SopGraph::from_sop(&req.sop);
    Json(graph).into_response()
}
