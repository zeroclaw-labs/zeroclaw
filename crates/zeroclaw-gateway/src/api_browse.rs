//! HTTP adapter over `zeroclaw_runtime::browse::list_directory`.

use axum::{
    Json,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use zeroclaw_runtime::browse::{
    BrowseEntry, BrowseError, delete_agent_workspace_path, list_agent_workspace, list_directory,
    make_agent_workspace_directory, make_directory, move_agent_workspace_path,
    read_agent_workspace_file, remove_directory,
};

use super::AppState;
use super::api::require_auth;

#[derive(Debug, Deserialize, Default)]
pub struct BrowseQuery {
    /// Path relative to `<install>/shared/`. Empty / unset = shared/ root.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrowseResponse {
    pub path: String,
    pub entries: Vec<BrowseEntry>,
}

/// `GET /api/browse?path=<relative-to-shared>`
pub async fn handle_browse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<BrowseQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let raw = q.path.unwrap_or_default();
    match list_directory(&config, &raw) {
        Ok(result) => Json(BrowseResponse {
            path: result.path,
            entries: result.entries,
        })
        .into_response(),
        Err(err) => browse_error_response(err),
    }
}

fn browse_error_response(err: BrowseError) -> Response {
    let status = match &err {
        BrowseError::Escape(_) => StatusCode::BAD_REQUEST,
        BrowseError::NotFound(_) => StatusCode::NOT_FOUND,
        BrowseError::NotADirectory(_) => StatusCode::BAD_REQUEST,
        BrowseError::Protected(_) => StatusCode::FORBIDDEN,
        BrowseError::ProtectedFile(_) => StatusCode::FORBIDDEN,
        BrowseError::TooLarge(_, _) => StatusCode::PAYLOAD_TOO_LARGE,
        BrowseError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({ "error": format!("{}", err) })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct BrowsePathBody {
    pub path: String,
}

/// `POST /api/browse/mkdir` — create a directory under `<install>/shared/`.
pub async fn handle_browse_mkdir(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BrowsePathBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    match make_directory(&config, &body.path) {
        Ok(()) => Json(serde_json::json!({ "created": body.path })).into_response(),
        Err(err) => browse_error_response(err),
    }
}

/// `DELETE /api/browse/rmdir` — recursively remove a directory under
/// `<install>/shared/`. Refuses protected top-level entries.
pub async fn handle_browse_rmdir(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BrowsePathBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    match remove_directory(&config, &body.path) {
        Ok(()) => Json(serde_json::json!({ "removed": body.path })).into_response(),
        Err(err) => browse_error_response(err),
    }
}

// ── Agent workspace ──────────────────────────────────────────────────────

/// `GET /api/agents/{alias}/workspace/list?path=<rel>` — one level under
/// `<install>/agents/{alias}/workspace/<rel>`.
pub async fn handle_agent_workspace_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(alias): AxumPath<String>,
    Query(q): Query<BrowseQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let raw = q.path.unwrap_or_default();
    match list_agent_workspace(&config, &alias, &raw) {
        Ok(result) => Json(BrowseResponse {
            path: result.path,
            entries: result.entries,
        })
        .into_response(),
        Err(err) => browse_error_response(err),
    }
}

#[derive(Debug, Serialize)]
pub struct FileReadResponse {
    pub path: String,
    pub size: u64,
    pub is_text: bool,
    /// UTF-8 text when `is_text` is true, base64 when false. Lets the
    /// dashboard render inline without a second round-trip for binary
    /// previews.
    pub content: String,
    pub encoding: &'static str,
}

/// `GET /api/agents/{alias}/workspace/read?path=<rel>` — read a single
/// file. Bounded by `AGENT_WORKSPACE_READ_CAP`.
pub async fn handle_agent_workspace_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(alias): AxumPath<String>,
    Query(q): Query<BrowseQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let raw = q.path.unwrap_or_default();
    match read_agent_workspace_file(&config, &alias, &raw) {
        Ok(result) => {
            let (content, encoding) = if result.is_text {
                (String::from_utf8(result.bytes).unwrap_or_default(), "utf8")
            } else {
                (
                    base64::engine::general_purpose::STANDARD.encode(&result.bytes),
                    "base64",
                )
            };
            Json(FileReadResponse {
                path: result.path,
                size: result.size,
                is_text: result.is_text,
                content,
                encoding,
            })
            .into_response()
        }
        Err(err) => browse_error_response(err),
    }
}

/// `DELETE /api/agents/{alias}/workspace/path` body `{ path: "<rel>" }`.
pub async fn handle_agent_workspace_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(alias): AxumPath<String>,
    Json(body): Json<BrowsePathBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    match delete_agent_workspace_path(&config, &alias, &body.path) {
        Ok(()) => Json(serde_json::json!({ "removed": body.path })).into_response(),
        Err(err) => browse_error_response(err),
    }
}

#[derive(Debug, Deserialize)]
pub struct MoveBody {
    pub from: String,
    pub to: String,
}

/// `POST /api/agents/{alias}/workspace/move` body `{ from, to }`.
pub async fn handle_agent_workspace_move(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(alias): AxumPath<String>,
    Json(body): Json<MoveBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    match move_agent_workspace_path(&config, &alias, &body.from, &body.to) {
        Ok(()) => Json(serde_json::json!({ "from": body.from, "to": body.to })).into_response(),
        Err(err) => browse_error_response(err),
    }
}

/// `POST /api/agents/{alias}/workspace/mkdir` body `{ path: "<rel>" }`.
pub async fn handle_agent_workspace_mkdir(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(alias): AxumPath<String>,
    Json(body): Json<BrowsePathBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    match make_agent_workspace_directory(&config, &alias, &body.path) {
        Ok(()) => Json(serde_json::json!({ "created": body.path })).into_response(),
        Err(err) => browse_error_response(err),
    }
}
