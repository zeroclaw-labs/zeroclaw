//! SOP authoring surface for the web node editor.
//!
//! `GET /api/sops` lists the on-disk SOPs; `GET /api/sops/:name/graph` returns
//! the inferred blueprint projection for one SOP. `POST /api/sops` creates,
//! `PUT /api/sops/:name` saves, `DELETE /api/sops/:name` removes. Every handler
//! resolves the sops dir from live config and calls the same
//! `zeroclaw_runtime::sop` functions the local RPC dispatch calls, so no
//! authoring logic is duplicated: both surfaces are thin skins over one
//! strict-validated runtime path. Gated by the standard `/api/*` bearer check.

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

/// GET /api/sops - list every SOP loadable from the configured directory.
pub async fn handle_sops_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let (dir, mode) = sops_dir_and_mode(&state);
    let sops = zeroclaw_runtime::sop::load_sops_from_directory(&dir, mode);
    Json(serde_json::json!({ "sops": sops })).into_response()
}

/// GET /api/sops/trigger-sources - the trigger-source registry the authoring
/// surfaces render. Thin skin over `zeroclaw_runtime::sop::registry_from_config`.
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

/// GET /api/sops/:name/graph - inferred blueprint projection for one SOP.
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

/// GET /api/sops/:name/runs/:run_id/overlay - run state projected onto the graph.
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

/// GET /api/sops/:name/full - the complete SOP definition for editing. The
/// graph projection omits step bodies and tools; the editor needs the full
/// `Sop`. Same load path as the graph route, serializing the SOP itself.
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

/// POST /api/sops - create a new SOP. Rejects an overwrite via the runtime's
/// `create_sop` guard. Body is the canonical `Sop` JSON.
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

/// PUT /api/sops/:name - save (create or overwrite) a SOP. The body name is the
/// authority; the path name is advisory. Strict-validated by `save_sop`.
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

/// DELETE /api/sops/:name - remove a SOP directory. 404 when absent.
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

/// Request body for `POST /api/sops/wire-draft`: an unsaved SOP draft plus one
/// edge mutation. The visual editor wires a draft that has not been persisted
/// yet, so the mutation applies in memory and nothing is written to disk.
#[derive(serde::Deserialize)]
pub struct WireDraftRequest {
    pub sop: zeroclaw_runtime::sop::Sop,
    pub edit: zeroclaw_runtime::sop::WireEdit,
}

/// POST /api/sops/wire-draft - apply one edge mutation to an in-memory SOP
/// draft and return the mutated draft plus its reprojected graph. Writes
/// nothing. The edge-kind-to-routing mapping is owned solely by
/// `zeroclaw_runtime::sop::apply_wire`; this handler only applies and reprojects.
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

/// Request body for `POST /api/sops/graph-draft`: an unsaved SOP draft.
#[derive(serde::Deserialize)]
pub struct GraphDraftRequest {
    pub sop: zeroclaw_runtime::sop::Sop,
}

/// POST /api/sops/graph-draft - reproject an in-memory SOP draft to its graph.
/// Writes nothing. The read-only counterpart to `wire-draft`: the visual editor
/// calls it after any non-wire field edit so the canvas reflects trigger
/// fan-in, data connections, pins, and layout without re-deriving graph shape.
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
