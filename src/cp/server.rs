use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio_util::io::ReaderStream;

use crate::cp::masking::{collect_key_paths, mask_config_secrets};
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

/// Read a tail window of `MAX_TAIL_BYTES` from a file and paginate within it.
/// Returns (lines, window_lines, has_more, truncated).
fn read_lines_paginated(
    path: &Path,
    offset: usize,
    count: usize,
) -> std::io::Result<(Vec<String>, usize, bool, bool)> {
    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();

    let read_from = file_len.saturating_sub(MAX_TAIL_BYTES);
    let truncated = read_from > 0;
    file.seek(SeekFrom::Start(read_from))?;

    let to_read = (file_len - read_from) as usize;
    let mut buf = vec![0u8; to_read];
    file.read_exact(&mut buf)?;

    let text = String::from_utf8_lossy(&buf);
    let all_lines: Vec<&str> = text.lines().collect();

    let skip = if truncated && !all_lines.is_empty() { 1 } else { 0 };
    let usable = &all_lines[skip..];
    let window_lines = usable.len();

    let start = offset.min(window_lines);
    let end = (start + count).min(window_lines);
    let lines: Vec<String> = usable[start..end].iter().map(|s| s.to_string()).collect();
    let has_more = end < window_lines;

    Ok((lines, window_lines, has_more, truncated))
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
        .route("/api/instances/:name/details", get(handle_details))
        .route("/api/instances/:name/tasks", get(handle_tasks))
        .route("/api/instances/:name/usage", get(handle_usage))
        .route(
            "/api/instances/:name/logs/download",
            get(handle_logs_download),
        )
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

fn instance_to_json(
    inst: &crate::db::Instance,
    live_status: &str,
    live_pid: Option<u32>,
) -> serde_json::Value {
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
            let (status, pid) =
                lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));
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
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
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
            let (status, pid) =
                lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));
            list.push(instance_to_json(inst, &status, pid));
        }

        Ok(serde_json::json!(list))
    })
    .await;

    match result {
        Ok(Ok(value)) => ok_json(value),
        Ok(Err(msg)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &msg),
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
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
                let (status, pid) =
                    lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));
                Ok(ok_json(instance_to_json(&inst, &status, pid)))
            }
            Ok(None) => Ok(err_json(
                StatusCode::NOT_FOUND,
                &format!("No instance named '{name}'"),
            )),
            Err(e) => Err(format!("{e:#}")),
        }
    })
    .await;

    match result {
        Ok(Ok(resp)) => resp,
        Ok(Err(msg)) => err_json(StatusCode::INTERNAL_SERVER_ERROR, &msg),
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
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
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
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
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
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
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
    }
}

// ── Logs (enhanced with pagination modes) ────────────────────────

/// Query params for the logs endpoint.
#[derive(Deserialize)]
struct LogsQuery {
    lines: Option<usize>,
    offset: Option<usize>,
    mode: Option<String>,
}

/// Maximum number of log lines returnable.
const MAX_LOG_LINES: usize = 10_000;

async fn handle_logs(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    let mode = query.mode.as_deref().unwrap_or("tail");

    // Validate mode
    if mode != "tail" && mode != "page" {
        return err_json(
            StatusCode::BAD_REQUEST,
            &format!("Invalid mode: '{mode}'. Valid values: tail, page"),
        );
    }

    let db_path = state.db_path.clone();
    let lines_count = query
        .lines
        .unwrap_or(lifecycle::DEFAULT_LOG_LINES)
        .min(MAX_LOG_LINES);
    let offset = query.offset.unwrap_or(0);
    let mode = mode.to_string();

    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        let instance = match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => inst,
            Ok(None) => {
                return err_json(
                    StatusCode::NOT_FOUND,
                    &format!("No instance named '{name}'"),
                )
            }
            Err(e) => {
                tracing::error!("Failed to query instance: {e:#}");
                return err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to query instance",
                );
            }
        };

        let inst_dir = lifecycle::instance_dir_from(&instance);
        let log_file = lifecycle::log_path(&inst_dir);

        if !log_file.exists() {
            return ok_json(serde_json::json!({
                "lines": [],
                "name": name,
                "mode": mode,
            }));
        }

        if mode == "page" {
            match read_lines_paginated(&log_file, offset, lines_count) {
                Ok((lines, window_lines, has_more, truncated)) => ok_json(serde_json::json!({
                    "lines": lines,
                    "name": name,
                    "mode": "page",
                    "offset": offset,
                    "window_lines": window_lines,
                    "has_more": has_more,
                    "truncated": truncated,
                })),
                Err(e) => {
                    tracing::error!("Failed to read log file: {e}");
                    err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to read log file",
                    )
                }
            }
        } else {
            match read_last_n_lines(&log_file, lines_count) {
                Ok(tail) => ok_json(serde_json::json!({
                    "lines": tail,
                    "name": name,
                    "mode": "tail",
                })),
                Err(e) => {
                    tracing::error!("Failed to read log file: {e}");
                    err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to read log file",
                    )
                }
            }
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
    }
}

// ── Logs download (streamed) ─────────────────────────────────────

async fn handle_logs_download(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    use futures_util::StreamExt;

    let db_path = state.db_path.clone();

    // Look up instance dir in a blocking task
    let lookup = tokio::task::spawn_blocking(move || -> Result<PathBuf, ApiResponse> {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return Err(resp),
        };
        match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => Ok(lifecycle::instance_dir_from(&inst)),
            Ok(None) => Err(err_json(
                StatusCode::NOT_FOUND,
                &format!("No instance named '{name}'"),
            )),
            Err(e) => {
                tracing::error!("Failed to query instance: {e:#}");
                Err(err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to query instance",
                ))
            }
        }
    })
    .await;

    let inst_dir = match lookup {
        Ok(Ok(p)) => p,
        Ok(Err((status, json))) => return (status, json).into_response(),
        Err(e) => {
            return err_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Task join error: {e}"),
            )
            .into_response()
        }
    };

    let log_path = lifecycle::log_path(&inst_dir);
    let rotated_path = lifecycle::rotated_log_path(&inst_dir);
    let has_current = log_path.exists();
    let has_rotated = rotated_path.exists();

    if !has_current && !has_rotated {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/plain")
            .header(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"daemon.log\"",
            )
            .body(Body::empty())
            .unwrap();
    }

    // Build a chained stream: rotated (older) first, then current (newer).
    // This gives chronological order in the downloaded file.
    let body = match (has_rotated, has_current) {
        (true, true) => {
            let rotated_file = match tokio::fs::File::open(&rotated_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to open rotated log: {e}");
                    return err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to open log file",
                    )
                    .into_response();
                }
            };
            let current_file = match tokio::fs::File::open(&log_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to open current log: {e}");
                    return err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to open log file",
                    )
                    .into_response();
                }
            };
            let rotated_stream = ReaderStream::new(rotated_file);
            let current_stream = ReaderStream::new(current_file);
            Body::from_stream(rotated_stream.chain(current_stream))
        }
        (true, false) => {
            let file = match tokio::fs::File::open(&rotated_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to open rotated log: {e}");
                    return err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to open log file",
                    )
                    .into_response();
                }
            };
            Body::from_stream(ReaderStream::new(file))
        }
        (false, true) => {
            let file = match tokio::fs::File::open(&log_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!("Failed to open current log: {e}");
                    return err_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to open log file",
                    )
                    .into_response();
                }
            };
            Body::from_stream(ReaderStream::new(file))
        }
        (false, false) => unreachable!(), // guarded above
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"daemon.log\"",
        )
        .body(body)
        .unwrap()
}

// ── Details endpoint ─────────────────────────────────────────────

async fn handle_details(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        let instance = match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => inst,
            Ok(None) => {
                return err_json(
                    StatusCode::NOT_FOUND,
                    &format!("No instance named '{name}'"),
                )
            }
            Err(e) => {
                tracing::error!("Failed to query instance: {e:#}");
                return err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to query instance",
                );
            }
        };

        let inst_dir = lifecycle::instance_dir_from(&instance);
        let (live_status, live_pid) =
            lifecycle::live_status(&inst_dir).unwrap_or(("unknown".to_string(), None));

        // Load and parse config
        let config_path = Path::new(&instance.config_path);
        let (config_json, config_error, config_unknown_fields) = if config_path.exists() {
            match std::fs::read_to_string(config_path) {
                Ok(raw_toml) => {
                    // Parse as raw TOML value
                    let raw_value: Result<toml::Value, _> = toml::from_str(&raw_toml);
                    // Parse as typed Config
                    let typed_result: Result<crate::config::schema::Config, _> =
                        toml::from_str(&raw_toml);

                    match typed_result {
                        Ok(typed_config) => {
                            let mut config_val =
                                serde_json::to_value(&typed_config).unwrap_or_default();
                            mask_config_secrets(&mut config_val);

                            // Compute unknown fields
                            let unknown_fields = if let Ok(raw_val) = raw_value {
                                let raw_json = serde_json::to_value(&raw_val).unwrap_or_default();
                                let raw_paths = collect_key_paths(&raw_json, "");
                                let typed_paths = collect_key_paths(&config_val, "");
                                let diff: Vec<String> = raw_paths
                                    .difference(&typed_paths)
                                    .cloned()
                                    .collect();
                                if diff.is_empty() {
                                    serde_json::Value::Array(vec![])
                                } else {
                                    serde_json::json!(diff)
                                }
                            } else {
                                serde_json::Value::Array(vec![])
                            };

                            (Some(config_val), None, unknown_fields)
                        }
                        Err(e) => (None, Some(format!("{e}")), serde_json::Value::Array(vec![])),
                    }
                }
                Err(e) => (
                    None,
                    Some(format!("Failed to read config: {e}")),
                    serde_json::Value::Array(vec![]),
                ),
            }
        } else {
            (
                None,
                Some("Config file not found".to_string()),
                serde_json::Value::Array(vec![]),
            )
        };

        // Extract identity info from config
        let identity = if let Some(ref cfg) = config_json {
            let format = cfg
                .get("identity")
                .and_then(|i| i.get("format"))
                .and_then(|f| f.as_str())
                .unwrap_or("openclaw");
            let configured = cfg.get("identity").is_some();
            serde_json::json!({
                "format": format,
                "configured": configured,
            })
        } else {
            serde_json::json!({ "format": "unknown", "configured": false })
        };

        // Extract channel info
        let channels = if let Some(ref cfg) = config_json {
            if let Some(cc) = cfg.get("channels_config") {
                let cli = cc
                    .get("cli")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let mut ch = serde_json::json!({ "cli": cli });
                for channel_name in &[
                    "telegram", "discord", "slack", "webhook", "imessage", "matrix", "whatsapp",
                    "email", "irc",
                ] {
                    if let Some(channel_val) = cc.get(*channel_name) {
                        if !channel_val.is_null() {
                            ch[channel_name] = serde_json::json!({ "configured": true });
                        }
                    }
                }
                ch
            } else {
                serde_json::json!({ "cli": true })
            }
        } else {
            serde_json::json!({ "cli": false })
        };

        // Extract model info
        let model = if let Some(ref cfg) = config_json {
            let provider = cfg
                .get("default_provider")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let model_name = cfg
                .get("default_model")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let routes_count = cfg
                .get("model_routes")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            serde_json::json!({
                "provider": provider,
                "model": model_name,
                "routes_count": routes_count,
            })
        } else {
            serde_json::json!({ "provider": "unknown", "model": "unknown", "routes_count": 0 })
        };

        // Runtime info
        let runtime = if let Some(ref cfg) = config_json {
            let kind = cfg
                .get("runtime")
                .and_then(|r| r.get("kind"))
                .and_then(|v| v.as_str())
                .unwrap_or("native");
            let docker_image = cfg
                .get("runtime")
                .and_then(|r| r.get("docker"))
                .and_then(|d| d.get("image"))
                .and_then(|v| v.as_str());
            serde_json::json!({
                "kind": kind,
                "docker_image": docker_image,
            })
        } else {
            serde_json::json!({ "kind": "unknown", "docker_image": null })
        };

        let mut response = serde_json::json!({
            "instance": {
                "id": instance.id,
                "name": instance.name,
                "port": instance.port,
                "status": live_status,
                "pid": live_pid,
            },
            "config": config_json,
            "config_error": config_error,
            "config_unknown_fields": config_unknown_fields,
            "identity": identity,
            "channels": channels,
            "model": model,
            "runtime": runtime,
        });

        // Remove null config_error for cleaner output
        if response.get("config_error").and_then(|v| v.as_null()).is_some() {
            if let Some(obj) = response.as_object_mut() {
                obj.remove("config_error");
            }
        }

        ok_json(response)
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
    }
}

// ── Tasks endpoint ───────────────────────────────────────────────

#[derive(Deserialize)]
struct TasksQuery {
    limit: Option<usize>,
    offset: Option<usize>,
    status: Option<String>,
    after: Option<String>,
    before: Option<String>,
}

async fn handle_tasks(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<TasksQuery>,
) -> impl IntoResponse {
    // Validate params
    let limit = query.limit.unwrap_or(20);
    if limit < 1 || limit > 1000 {
        return err_json(
            StatusCode::BAD_REQUEST,
            "Invalid limit: must be between 1 and 1000",
        );
    }
    let offset = query.offset.unwrap_or(0);
    if offset > 100_000 {
        return err_json(
            StatusCode::BAD_REQUEST,
            "Invalid offset: must be at most 100000",
        );
    }
    if let Some(ref status) = query.status {
        if !["started", "completed", "failed"].contains(&status.as_str()) {
            return err_json(
                StatusCode::BAD_REQUEST,
                &format!(
                    "Invalid status: '{status}'. Valid values: started, completed, failed"
                ),
            );
        }
    }

    let db_path = state.db_path.clone();
    let status_filter = query.status.clone();
    let after = query.after.clone();
    let before = query.before.clone();

    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        let instance = match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => inst,
            Ok(None) => {
                return err_json(
                    StatusCode::NOT_FOUND,
                    &format!("No instance named '{name}'"),
                )
            }
            Err(e) => {
                tracing::error!("Failed to query instance: {e:#}");
                return err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to query instance",
                );
            }
        };

        match registry.list_agent_events(
            &instance.id,
            limit,
            offset,
            status_filter.as_deref(),
            after.as_deref(),
            before.as_deref(),
        ) {
            Ok((events, total)) => {
                let data_available = !events.is_empty() || total > 0;
                let tasks: Vec<serde_json::Value> = events
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "id": e.id,
                            "instance_id": e.instance_id,
                            "event_type": e.event_type,
                            "channel": e.channel,
                            "summary": e.summary,
                            "status": e.status,
                            "duration_ms": e.duration_ms,
                            "correlation_id": e.correlation_id,
                            "created_at": e.created_at,
                        })
                    })
                    .collect();

                let mut resp = serde_json::json!({
                    "tasks": tasks,
                    "total": total,
                    "data_available": data_available,
                    "limit": limit,
                    "offset": offset,
                });
                if !data_available {
                    resp["message"] = serde_json::json!("No event data available.");
                }
                ok_json(resp)
            }
            Err(e) => {
                tracing::error!("Failed to list agent events: {e:#}");
                err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to list agent events",
                )
            }
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
    }
}

// ── Usage endpoint ───────────────────────────────────────────────

#[derive(Deserialize)]
struct UsageQuery {
    window: Option<String>,
}

async fn handle_usage(
    State(state): State<CpState>,
    AxumPath(name): AxumPath<String>,
    Query(query): Query<UsageQuery>,
) -> impl IntoResponse {
    let window = query.window.as_deref().unwrap_or("24h");
    if !["1h", "24h", "7d", "30d"].contains(&window) {
        return err_json(
            StatusCode::BAD_REQUEST,
            &format!("Invalid window: '{window}'. Valid values: 1h, 24h, 7d, 30d"),
        );
    }

    let db_path = state.db_path.clone();
    let window = window.to_string();

    let result = tokio::task::spawn_blocking(move || -> ApiResponse {
        let registry = match open_registry(&db_path) {
            Ok(r) => r,
            Err(resp) => return resp,
        };

        let instance = match registry.get_instance_by_name(&name) {
            Ok(Some(inst)) => inst,
            Ok(None) => {
                return err_json(
                    StatusCode::NOT_FOUND,
                    &format!("No instance named '{name}'"),
                )
            }
            Err(e) => {
                tracing::error!("Failed to query instance: {e:#}");
                return err_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to query instance",
                );
            }
        };

        // Compute window start from now
        let now = chrono::Utc::now();
        let duration = match window.as_str() {
            "1h" => chrono::Duration::hours(1),
            "24h" => chrono::Duration::hours(24),
            "7d" => chrono::Duration::days(7),
            "30d" => chrono::Duration::days(30),
            _ => unreachable!(),
        };
        let window_start = (now - duration).format("%Y-%m-%d %H:%M:%S").to_string();
        let window_end = now.format("%Y-%m-%d %H:%M:%S").to_string();

        match registry.get_agent_usage(&instance.id, Some(&window_start), Some(&window_end)) {
            Ok(summary) => {
                let data_available = summary.request_count > 0;
                ok_json(serde_json::json!({
                    "instance_name": name,
                    "window": window,
                    "data_available": data_available,
                    "usage": {
                        "input_tokens": summary.input_tokens,
                        "output_tokens": summary.output_tokens,
                        "total_tokens": summary.total_tokens,
                        "request_count": summary.request_count,
                        "unknown_count": summary.unknown_count,
                    },
                }))
            }
            Err(e) => {
                tracing::error!("Failed to query usage: {e:#}");
                err_json(StatusCode::INTERNAL_SERVER_ERROR, "Failed to query usage")
            }
        }
    })
    .await;

    match result {
        Ok(resp) => resp,
        Err(e) => err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Task join error: {e}"),
        ),
    }
}
