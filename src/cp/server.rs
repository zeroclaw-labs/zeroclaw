use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::db::Registry;
use crate::lifecycle;
use crate::lifecycle::LifecycleError;

/// Maximum bytes to read from the tail of a log file.
/// Bounds memory usage regardless of total file size.
const MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB

/// Read the last `n` lines from a file without loading the entire file.
/// Reads at most `MAX_TAIL_BYTES` from the end of the file.
fn read_last_n_lines(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();

    let read_from = file_len.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(read_from))?;

    let to_read = (file_len - read_from) as usize;
    let mut buf = vec![0u8; to_read];
    file.read_exact(&mut buf)?;

    let text = String::from_utf8_lossy(&buf);
    let all_lines: Vec<&str> = text.lines().collect();

    // If we seeked past the start, the first "line" may be partial -- skip it
    let skip = if read_from > 0 && !all_lines.is_empty() {
        1
    } else {
        0
    };

    let usable = &all_lines[skip..];
    let start = usable.len().saturating_sub(n);
    Ok(usable[start..].iter().map(|s| s.to_string()).collect())
}

/// Shared state: just the DB path. Each request opens its own connection.
#[derive(Clone)]
pub struct CpState {
    pub db_path: Arc<PathBuf>,
}

/// Build the axum router with all CP API routes.
pub fn build_router(state: CpState) -> Router {
    Router::new()
        .route("/api/health", get(handle_health))
        .route("/api/instances", get(handle_list_instances))
        .route("/api/instances/:name", get(handle_get_instance))
        .route("/api/instances/:name/start", post(handle_start))
        .route("/api/instances/:name/stop", post(handle_stop))
        .route("/api/instances/:name/restart", post(handle_restart))
        .route("/api/instances/:name/logs", get(handle_logs))
        .with_state(state)
}

// ── Response helpers ─────────────────────────────────────────────

type ApiResponse = (StatusCode, Json<serde_json::Value>);

fn ok_json(value: serde_json::Value) -> ApiResponse {
    (StatusCode::OK, Json(value))
}

fn err_json(status: StatusCode, message: &str) -> ApiResponse {
    (status, Json(serde_json::json!({ "error": message })))
}

fn lifecycle_err_to_response(e: LifecycleError) -> ApiResponse {
    match &e {
        LifecycleError::NotFound(_) => err_json(StatusCode::NOT_FOUND, &e.to_string()),
        LifecycleError::AlreadyRunning(_) => err_json(StatusCode::CONFLICT, &e.to_string()),
        LifecycleError::NotRunning(_) => err_json(StatusCode::CONFLICT, &e.to_string()),
        LifecycleError::LockHeld => err_json(StatusCode::SERVICE_UNAVAILABLE, &e.to_string()),
        LifecycleError::Internal(_) => {
            err_json(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())
        }
    }
}

fn open_registry(db_path: &Path) -> Result<Registry, ApiResponse> {
    Registry::open(db_path).map_err(|e| {
        tracing::error!("Failed to open registry: {e:#}");
        err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to open registry",
        )
    })
}

// ── Instance serialization ───────────────────────────────────────

fn instance_to_json(inst: &crate::db::Instance, live_status: &str, live_pid: Option<u32>) -> serde_json::Value {
    serde_json::json!({
        "id": inst.id,
        "name": inst.name,
        "port": inst.port,
        "status": live_status,
        "pid": live_pid,
        "config_path": inst.config_path,
        "workspace_dir": inst.workspace_dir,
    })
}

// ── Handlers ─────────────────────────────────────────────────────

async fn handle_health(State(state): State<CpState>) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let registry = Registry::open(&db_path).map_err(|e| format!("{e:#}"))?;
        let instances = registry.list_instances().map_err(|e| format!("{e:#}"))?;

        let mut instance_map = serde_json::Map::new();
        for inst in &instances {
            let inst_dir = lifecycle::instance_dir_from(inst);
            let (status, pid) = lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));
            instance_map.insert(
                inst.name.clone(),
                serde_json::json!({ "status": status, "pid": pid }),
            );
        }

        Ok(serde_json::json!({
            "status": "ok",
            "instances": instance_map,
        }))
    })
    .await;

    match result {
        Ok(Ok(value)) => ok_json(value),
        Ok(Err(msg)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &msg),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}

async fn handle_list_instances(State(state): State<CpState>) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let registry = Registry::open(&db_path).map_err(|e| format!("{e:#}"))?;
        let instances = registry.list_instances().map_err(|e| format!("{e:#}"))?;

        let mut list = Vec::new();
        for inst in &instances {
            let inst_dir = lifecycle::instance_dir_from(inst);
            let (status, pid) = lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));
            list.push(instance_to_json(inst, &status, pid));
        }

        Ok(serde_json::json!(list))
    })
    .await;

    match result {
        Ok(Ok(value)) => ok_json(value),
        Ok(Err(msg)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &msg),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}

async fn handle_get_instance(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<ApiResponse, String> {
        let registry = Registry::open(&db_path).map_err(|e| format!("{e:#}"))?;
        match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => {
                let inst_dir = lifecycle::instance_dir_from(&inst);
                let (status, pid) = lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));
                Ok(ok_json(instance_to_json(&inst, &status, pid)))
            }
            Ok(None) => Ok(err_json(StatusCode::NOT_FOUND, &format!("No instance named '{name}'"))),
            Err(e) => Err(format!("{e:#}")),
        }
    })
    .await;

    match result {
        Ok(Ok(resp)) => resp,
        Ok(Err(msg)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &msg),
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}

async fn handle_start(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };
        match lifecycle::start_instance(&registry, &name) {
            Ok(()) => ok_json(serde_json::json!({ "status": "started", "name": name })),
            Err(e) => lifecycle_err_to_response(e),
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}

async fn handle_stop(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };
        match lifecycle::stop_instance(&registry, &name) {
            Ok(()) => ok_json(serde_json::json!({ "status": "stopped", "name": name })),
            Err(e) => lifecycle_err_to_response(e),
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}

async fn handle_restart(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };
        match lifecycle::restart_instance(&registry, &name) {
            Ok(()) => ok_json(serde_json::json!({ "status": "restarted", "name": name })),
            Err(e) => lifecycle_err_to_response(e),
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}

/// Query params for the logs endpoint.
#[derive(Deserialize)]
struct LogsQuery {
    lines: Option<usize>,
}

/// Maximum number of log lines returnable.
const MAX_LOG_LINES: usize = 10_000;

async fn handle_logs(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let lines = query
        .lines
        .unwrap_or(lifecycle::DEFAULT_LOG_LINES)
        .min(MAX_LOG_LINES);

    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        let instance = match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => inst,
            Ok(None) => return err_json(StatusCode::NOT_FOUND, &format!("No instance named '{name}'")),
            Err(e) => {
                tracing::error!("Failed to query instance: {e:#}");
                return err_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to query instance");
            }
        };

        let inst_dir = lifecycle::instance_dir_from(&instance);
        let log_file = lifecycle::log_path(&inst_dir);

        if !log_file.exists() {
            return ok_json(serde_json::json!({ "lines": [], "name": name }));
        }

        match read_last_n_lines(&log_file, lines) {
            Ok(tail) => ok_json(serde_json::json!({ "lines": tail, "name": name })),
            Err(e) => {
                tracing::error!("Failed to read log file: {e}");
                err_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read log file")
            }
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &format!("Task join error: {e}")),
    }
}
