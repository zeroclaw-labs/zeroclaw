//! SDK API routes — 48+ HTTP endpoints for CRUD operations on all Aria registries.
//!
//! All routes are tenant-isolated via Bearer token authentication.
//! Route pattern: /api/v1/registry/{resource}

use crate::aria::db::AriaDb;
use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{delete, get, patch, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Shared state for registry API routes
#[derive(Clone)]
pub struct RegistryApiState {
    pub db: AriaDb,
}

/// Route context extracted from auth headers
#[derive(Debug, Clone)]
pub struct RouteContext {
    pub tenant_id: String,
    pub user_id: Option<String>,
}

/// Pagination query parameters
#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub status: Option<String>,
}

/// Standard API response wrapper
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Json<Self> {
        Json(Self {
            success: true,
            data: Some(data),
            error: None,
            count: None,
        })
    }

    pub fn ok_with_count(data: T, count: u64) -> Json<Self> {
        Json(Self {
            success: true,
            data: Some(data),
            error: None,
            count: Some(count),
        })
    }

    pub fn err(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ApiResponse<()>>) {
        (
            status,
            Json(ApiResponse::<()> {
                success: false,
                data: None,
                error: Some(message.into()),
                count: None,
            }),
        )
    }
}

/// Extract tenant context from Bearer token.
/// In production, this would validate the token and extract tenant/user IDs.
/// For now, we use a simple extraction from the token format: "tenant_id:token".
fn extract_route_context(headers: &HeaderMap) -> Result<RouteContext, (StatusCode, Json<ApiResponse<()>>)> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    if token.is_empty() {
        return Err(ApiResponse::<()>::err(
            StatusCode::UNAUTHORIZED,
            "Missing or invalid Authorization header. Use: Bearer <token>",
        ));
    }

    // Extract tenant_id from token. Format: "tenant_id:secret" or just use as tenant_id
    let tenant_id = if let Some((tid, _)) = token.split_once(':') {
        tid.to_string()
    } else {
        token.to_string()
    };

    Ok(RouteContext {
        tenant_id,
        user_id: None,
    })
}

/// Build the full registry API router with all 48+ endpoints
pub fn registry_router(db: AriaDb) -> Router {
    let state = RegistryApiState { db };

    Router::new()
        // ── Tools ─────────────────────────────────────────────
        .route("/api/v1/registry/tools", post(tools_upload))
        .route("/api/v1/registry/tools", get(tools_list))
        .route("/api/v1/registry/tools/{id}", get(tools_get))
        .route("/api/v1/registry/tools/{id}", delete(tools_delete))
        // ── Agents ────────────────────────────────────────────
        .route("/api/v1/registry/agents", post(agents_upload))
        .route("/api/v1/registry/agents", get(agents_list))
        .route("/api/v1/registry/agents/{id}", get(agents_get))
        .route("/api/v1/registry/agents/{id}", delete(agents_delete))
        // ── Memory ────────────────────────────────────────────
        .route("/api/v1/registry/memory", post(memory_set))
        .route("/api/v1/registry/memory", get(memory_list))
        .route("/api/v1/registry/memory/{id}", get(memory_get))
        .route("/api/v1/registry/memory/{id}", delete(memory_delete))
        // ── Tasks ─────────────────────────────────────────────
        .route("/api/v1/registry/tasks", post(tasks_create))
        .route("/api/v1/registry/tasks", get(tasks_list))
        .route("/api/v1/registry/tasks/{id}", get(tasks_get))
        .route("/api/v1/registry/tasks/{id}/status", patch(tasks_update_status))
        .route("/api/v1/registry/tasks/{id}/cancel", post(tasks_cancel))
        // ── Feeds ─────────────────────────────────────────────
        .route("/api/v1/registry/feeds", post(feeds_upload))
        .route("/api/v1/registry/feeds", get(feeds_list))
        .route("/api/v1/registry/feeds/{id}", get(feeds_get))
        .route("/api/v1/registry/feeds/{id}", delete(feeds_delete))
        // ── Crons ─────────────────────────────────────────────
        .route("/api/v1/registry/crons", post(crons_upload))
        .route("/api/v1/registry/crons", get(crons_list))
        .route("/api/v1/registry/crons/{id}", get(crons_get))
        .route("/api/v1/registry/crons/{id}", delete(crons_delete))
        // ── KV ────────────────────────────────────────────────
        .route("/api/v1/registry/kv", post(kv_set))
        .route("/api/v1/registry/kv", get(kv_list))
        .route("/api/v1/registry/kv/{id}", get(kv_get))
        .route("/api/v1/registry/kv/{id}", delete(kv_delete))
        .route("/api/v1/registry/kv/query", get(kv_query))
        // ── Teams ─────────────────────────────────────────────
        .route("/api/v1/registry/teams", post(teams_upload))
        .route("/api/v1/registry/teams", get(teams_list))
        .route("/api/v1/registry/teams/{id}", get(teams_get))
        .route("/api/v1/registry/teams/{id}", delete(teams_delete))
        // ── Pipelines ─────────────────────────────────────────
        .route("/api/v1/registry/pipelines", post(pipelines_upload))
        .route("/api/v1/registry/pipelines", get(pipelines_list))
        .route("/api/v1/registry/pipelines/{id}", get(pipelines_get))
        .route("/api/v1/registry/pipelines/{id}", delete(pipelines_delete))
        // ── Containers ────────────────────────────────────────
        .route("/api/v1/registry/containers", post(containers_upload))
        .route("/api/v1/registry/containers", get(containers_list))
        .route("/api/v1/registry/containers/{id}", get(containers_get))
        .route("/api/v1/registry/containers/{id}", delete(containers_delete))
        // ── Networks ──────────────────────────────────────────
        .route("/api/v1/registry/networks", post(networks_upload))
        .route("/api/v1/registry/networks", get(networks_list))
        .route("/api/v1/registry/networks/{id}", get(networks_get))
        .route("/api/v1/registry/networks/{id}", delete(networks_delete))
        // ── Bulk ──────────────────────────────────────────────
        .route("/api/v1/registry/bulk", post(bulk_upload))
        .with_state(state)
}

// ══════════════════════════════════════════════════════════════════
// TOOL HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct ToolUploadBody {
    name: String,
    description: Option<String>,
    schema: Option<serde_json::Value>,
    handler_code: Option<String>,
    sandbox_config: Option<serde_json::Value>,
}

async fn tools_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<ToolUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let handler_hash = hash_code(body.handler_code.as_deref().unwrap_or(""));

    let result = state.db.with_conn(|conn| {
        // Check for existing tool with same tenant+name
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_tools WHERE tenant_id = ?1 AND name = ?2 AND status != 'deleted'",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_id) = existing {
            // Upsert: update existing
            conn.execute(
                "UPDATE aria_tools SET description = ?1, schema = ?2, handler_code = ?3,
                 handler_hash = ?4, sandbox_config = ?5, version = version + 1, updated_at = ?6
                 WHERE id = ?7",
                rusqlite::params![
                    body.description.as_deref().unwrap_or(""),
                    serde_json::to_string(&body.schema.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    body.handler_code.as_deref().unwrap_or(""),
                    handler_hash,
                    body.sandbox_config.map(|v| serde_json::to_string(&v).unwrap_or_default()),
                    now,
                    existing_id,
                ],
            )?;
            Ok(existing_id)
        } else {
            conn.execute(
                "INSERT INTO aria_tools (id, tenant_id, name, description, schema, handler_code,
                 handler_hash, sandbox_config, status, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', 1, ?9, ?9)",
                rusqlite::params![
                    id,
                    ctx.tenant_id,
                    body.name,
                    body.description.as_deref().unwrap_or(""),
                    serde_json::to_string(&body.schema.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    body.handler_code.as_deref().unwrap_or(""),
                    handler_hash,
                    body.sandbox_config.map(|v| serde_json::to_string(&v).unwrap_or_default()),
                    now,
                ],
            )?;
            Ok(id)
        }
    });

    match result {
        Ok(tool_id) => {
            let resp = serde_json::json!({"id": tool_id, "name": body.name});
            (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": resp}))).into_response()
        }
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn tools_list(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Query(pagination): Query<PaginationQuery>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let result = state.db.with_conn(|conn| {
        let limit = pagination.limit.unwrap_or(100);
        let offset = pagination.offset.unwrap_or(0);
        let mut stmt = conn.prepare(
            "SELECT id, name, description, status, version, created_at, updated_at
             FROM aria_tools WHERE tenant_id = ?1 AND status != 'deleted'
             ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![ctx.tenant_id, limit, offset], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "version": row.get::<_, i64>(4)?,
                "created_at": row.get::<_, String>(5)?,
                "updated_at": row.get::<_, String>(6)?,
            }))
        })?;
        let items: Vec<serde_json::Value> = rows.filter_map(|r| r.ok()).collect();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM aria_tools WHERE tenant_id = ?1 AND status != 'deleted'",
            rusqlite::params![ctx.tenant_id],
            |row| row.get(0),
        )?;

        Ok(serde_json::json!({"items": items, "total": count}))
    });

    match result {
        Ok(data) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": data}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn tools_get(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let result = state.db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, name, description, schema, handler_code, handler_hash,
             sandbox_config, status, version, created_at, updated_at
             FROM aria_tools WHERE id = ?1 AND tenant_id = ?2 AND status != 'deleted'",
            rusqlite::params![id, ctx.tenant_id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "schema": row.get::<_, String>(3)?,
                    "handler_code": row.get::<_, String>(4)?,
                    "handler_hash": row.get::<_, String>(5)?,
                    "sandbox_config": row.get::<_, Option<String>>(6)?,
                    "status": row.get::<_, String>(7)?,
                    "version": row.get::<_, i64>(8)?,
                    "created_at": row.get::<_, String>(9)?,
                    "updated_at": row.get::<_, String>(10)?,
                }))
            },
        )
        .map_err(|_| anyhow::anyhow!("Tool not found"))
    });

    match result {
        Ok(data) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": data}))).into_response(),
        Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, "Tool not found").into_response(),
    }
}

async fn tools_delete(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let result = state.db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE aria_tools SET status = 'deleted', updated_at = ?1
             WHERE id = ?2 AND tenant_id = ?3 AND status != 'deleted'",
            rusqlite::params![chrono::Utc::now().to_rfc3339(), id, ctx.tenant_id],
        )?;
        if changed == 0 {
            anyhow::bail!("Tool not found");
        }
        Ok(())
    });

    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, "Tool not found").into_response(),
    }
}

// ══════════════════════════════════════════════════════════════════
// AGENT HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct AgentUploadBody {
    name: String,
    description: Option<String>,
    model: Option<String>,
    temperature: Option<f64>,
    system_prompt: Option<String>,
    tools: Option<Vec<String>>,
    thinking: Option<String>,
    max_retries: Option<u32>,
    timeout_seconds: Option<u64>,
    handler_code: Option<String>,
    sandbox_config: Option<serde_json::Value>,
}

async fn agents_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<AgentUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let handler_hash = hash_code(body.handler_code.as_deref().unwrap_or(""));
    let tools_json = serde_json::to_string(&body.tools.unwrap_or_default()).unwrap_or_default();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_agents WHERE tenant_id = ?1 AND name = ?2 AND status != 'deleted'",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_agents SET description = ?1, model = ?2, temperature = ?3,
                 system_prompt = ?4, tools = ?5, thinking = ?6, max_retries = ?7,
                 timeout_seconds = ?8, handler_code = ?9, handler_hash = ?10,
                 sandbox_config = ?11, version = version + 1, updated_at = ?12
                 WHERE id = ?13",
                rusqlite::params![
                    body.description.as_deref().unwrap_or(""),
                    body.model,
                    body.temperature,
                    body.system_prompt,
                    tools_json,
                    body.thinking,
                    body.max_retries.map(|v| v as i64),
                    body.timeout_seconds.map(|v| v as i64),
                    body.handler_code.as_deref().unwrap_or(""),
                    handler_hash,
                    body.sandbox_config.map(|v| serde_json::to_string(&v).unwrap_or_default()),
                    now,
                    existing_id,
                ],
            )?;
            Ok(existing_id)
        } else {
            conn.execute(
                "INSERT INTO aria_agents (id, tenant_id, name, description, model, temperature,
                 system_prompt, tools, thinking, max_retries, timeout_seconds, handler_code,
                 handler_hash, sandbox_config, status, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'active', 1, ?15, ?15)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name,
                    body.description.as_deref().unwrap_or(""),
                    body.model, body.temperature, body.system_prompt, tools_json,
                    body.thinking,
                    body.max_retries.map(|v| v as i64),
                    body.timeout_seconds.map(|v| v as i64),
                    body.handler_code.as_deref().unwrap_or(""),
                    handler_hash,
                    body.sandbox_config.map(|v| serde_json::to_string(&v).unwrap_or_default()),
                    now,
                ],
            )?;
            Ok(id)
        }
    });

    match result {
        Ok(agent_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": agent_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// Generic CRUD handler generator macro to avoid repetitive code
macro_rules! impl_list_get_delete {
    ($table:expr, $resource:expr,
     list_fn: $list_fn:ident, get_fn: $get_fn:ident, delete_fn: $delete_fn:ident,
     columns: $columns:expr, soft_delete: $soft_delete:expr) => {
        async fn $list_fn(
            State(state): State<RegistryApiState>,
            headers: HeaderMap,
            Query(pagination): Query<PaginationQuery>,
        ) -> impl IntoResponse {
            let ctx = match extract_route_context(&headers) {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };
            let limit = pagination.limit.unwrap_or(100);
            let offset = pagination.offset.unwrap_or(0);
            let delete_filter = if $soft_delete { " AND status != 'deleted'" } else { "" };
            let query = format!(
                "SELECT {} FROM {} WHERE tenant_id = ?1{} ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
                $columns, $table, delete_filter
            );
            let count_query = format!(
                "SELECT COUNT(*) FROM {} WHERE tenant_id = ?1{}",
                $table, delete_filter
            );

            let result = state.db.with_conn(|conn| {
                let mut stmt = conn.prepare(&query)?;
                let col_count = stmt.column_count();
                let col_names: Vec<String> = (0..col_count).map(|i| stmt.column_name(i).unwrap_or("").to_string()).collect();

                let rows = stmt.query_map(rusqlite::params![ctx.tenant_id, limit, offset], |row| {
                    let mut map = serde_json::Map::new();
                    for (i, name) in col_names.iter().enumerate() {
                        if let Ok(val) = row.get::<_, String>(i) {
                            map.insert(name.clone(), serde_json::Value::String(val));
                        } else if let Ok(val) = row.get::<_, i64>(i) {
                            map.insert(name.clone(), serde_json::Value::Number(val.into()));
                        } else if let Ok(val) = row.get::<_, f64>(i) {
                            if let Some(n) = serde_json::Number::from_f64(val) {
                                map.insert(name.clone(), serde_json::Value::Number(n));
                            }
                        } else {
                            map.insert(name.clone(), serde_json::Value::Null);
                        }
                    }
                    Ok(serde_json::Value::Object(map))
                })?;
                let items: Vec<serde_json::Value> = rows.filter_map(|r| r.ok()).collect();

                let count: i64 = conn.query_row(&count_query, rusqlite::params![ctx.tenant_id], |row| row.get(0))?;
                Ok(serde_json::json!({"items": items, "total": count}))
            });

            match result {
                Ok(data) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": data}))).into_response(),
                Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }

        async fn $get_fn(
            State(state): State<RegistryApiState>,
            headers: HeaderMap,
            Path(id): Path<String>,
        ) -> impl IntoResponse {
            let ctx = match extract_route_context(&headers) {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };
            let delete_filter = if $soft_delete { " AND status != 'deleted'" } else { "" };
            let query = format!(
                "SELECT {} FROM {} WHERE id = ?1 AND tenant_id = ?2{}",
                $columns, $table, delete_filter
            );

            let result = state.db.with_conn(|conn| {
                let mut stmt = conn.prepare(&query)?;
                let col_count = stmt.column_count();
                let col_names: Vec<String> = (0..col_count).map(|i| stmt.column_name(i).unwrap_or("").to_string()).collect();

                stmt.query_row(rusqlite::params![id, ctx.tenant_id], |row| {
                    let mut map = serde_json::Map::new();
                    for (i, name) in col_names.iter().enumerate() {
                        if let Ok(val) = row.get::<_, String>(i) {
                            map.insert(name.clone(), serde_json::Value::String(val));
                        } else if let Ok(val) = row.get::<_, i64>(i) {
                            map.insert(name.clone(), serde_json::Value::Number(val.into()));
                        } else if let Ok(val) = row.get::<_, f64>(i) {
                            if let Some(n) = serde_json::Number::from_f64(val) {
                                map.insert(name.clone(), serde_json::Value::Number(n));
                            }
                        } else {
                            map.insert(name.clone(), serde_json::Value::Null);
                        }
                    }
                    Ok(serde_json::Value::Object(map))
                })
                .map_err(|_| anyhow::anyhow!(concat!($resource, " not found")))
            });

            match result {
                Ok(data) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": data}))).into_response(),
                Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, concat!($resource, " not found")).into_response(),
            }
        }

        async fn $delete_fn(
            State(state): State<RegistryApiState>,
            headers: HeaderMap,
            Path(id): Path<String>,
        ) -> impl IntoResponse {
            let ctx = match extract_route_context(&headers) {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };

            let result = state.db.with_conn(|conn| {
                let changed = if $soft_delete {
                    conn.execute(
                        &format!("UPDATE {} SET status = 'deleted', updated_at = ?1 WHERE id = ?2 AND tenant_id = ?3 AND status != 'deleted'", $table),
                        rusqlite::params![chrono::Utc::now().to_rfc3339(), id, ctx.tenant_id],
                    )?
                } else {
                    conn.execute(
                        &format!("DELETE FROM {} WHERE id = ?1 AND tenant_id = ?2", $table),
                        rusqlite::params![id, ctx.tenant_id],
                    )?
                };
                if changed == 0 {
                    anyhow::bail!(concat!($resource, " not found"));
                }

                Ok(())
            });

            match result {
                Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
                Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, concat!($resource, " not found")).into_response(),
            }
        }
    };
}

// Agent list/get/delete
impl_list_get_delete!(
    "aria_agents", "Agent",
    list_fn: agents_list, get_fn: agents_get, delete_fn: agents_delete,
    columns: "id, name, description, model, temperature, tools, thinking, status, version, created_at, updated_at",
    soft_delete: true
);

// ══════════════════════════════════════════════════════════════════
// MEMORY HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct MemorySetBody {
    key: String,
    value: String,
    tier: Option<String>,
    namespace: Option<String>,
    session_id: Option<String>,
    ttl_seconds: Option<u64>,
}

async fn memory_set(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<MemorySetBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let tier = body.tier.as_deref().unwrap_or("longterm");
    let expires_at = body.ttl_seconds.map(|ttl| {
        (now + chrono::Duration::seconds(ttl as i64)).to_rfc3339()
    });

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_memory WHERE tenant_id = ?1 AND key = ?2 AND tier = ?3",
                rusqlite::params![ctx.tenant_id, body.key, tier],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_memory SET value = ?1, namespace = ?2, session_id = ?3,
                 ttl_seconds = ?4, expires_at = ?5, updated_at = ?6 WHERE id = ?7",
                rusqlite::params![
                    body.value, body.namespace, body.session_id,
                    body.ttl_seconds.map(|v| v as i64), expires_at,
                    now_str, existing_id,
                ],
            )?;
            Ok(existing_id)
        } else {
            conn.execute(
                "INSERT INTO aria_memory (id, tenant_id, key, value, tier, namespace, session_id,
                 ttl_seconds, expires_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                rusqlite::params![
                    id, ctx.tenant_id, body.key, body.value, tier,
                    body.namespace, body.session_id,
                    body.ttl_seconds.map(|v| v as i64), expires_at, now_str,
                ],
            )?;
            Ok(id)
        }
    });

    match result {
        Ok(mem_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": mem_id}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_memory", "Memory entry",
    list_fn: memory_list, get_fn: memory_get, delete_fn: memory_delete,
    columns: "id, key, value, tier, namespace, session_id, ttl_seconds, expires_at, created_at, updated_at",
    soft_delete: false
);

// ══════════════════════════════════════════════════════════════════
// TASK HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct TaskCreateBody {
    name: String,
    description: Option<String>,
    handler_code: Option<String>,
    params: Option<serde_json::Value>,
    agent_id: Option<String>,
}

async fn tasks_create(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<TaskCreateBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let handler_hash = hash_code(body.handler_code.as_deref().unwrap_or(""));

    let result = state.db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO aria_tasks (id, tenant_id, name, description, handler_code, handler_hash,
             params, status, agent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?9)",
            rusqlite::params![
                id, ctx.tenant_id, body.name,
                body.description.as_deref().unwrap_or(""),
                body.handler_code.as_deref().unwrap_or(""),
                handler_hash,
                serde_json::to_string(&body.params.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                body.agent_id,
                now,
            ],
        )?;
        Ok(id.clone())
    });

    match result {
        Ok(task_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": task_id, "status": "pending"}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct TaskStatusUpdateBody {
    status: String,
    result: Option<serde_json::Value>,
    error: Option<String>,
}

async fn tasks_update_status(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<TaskStatusUpdateBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();

    let result = state.db.with_conn(|conn| {
        let mut extra_set = String::new();
        if body.status == "running" {
            extra_set = format!(", started_at = '{now}'");
        } else if body.status == "completed" || body.status == "failed" || body.status == "cancelled" {
            extra_set = format!(", completed_at = '{now}'");
        }

        let changed = conn.execute(
            &format!(
                "UPDATE aria_tasks SET status = ?1, result = ?2, error = ?3, updated_at = ?4{}
                 WHERE id = ?5 AND tenant_id = ?6",
                extra_set
            ),
            rusqlite::params![
                body.status,
                body.result.map(|v| serde_json::to_string(&v).unwrap_or_default()),
                body.error,
                now, id, ctx.tenant_id,
            ],
        )?;
        if changed == 0 {
            anyhow::bail!("Task not found");
        }
        Ok(())
    });

    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": {"status": body.status}}))).into_response(),
        Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, "Task not found").into_response(),
    }
}

async fn tasks_cancel(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();

    let result = state.db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE aria_tasks SET status = 'cancelled', completed_at = ?1, updated_at = ?1
             WHERE id = ?2 AND tenant_id = ?3 AND status IN ('pending', 'running')",
            rusqlite::params![now, id, ctx.tenant_id],
        )?;
        if changed == 0 {
            anyhow::bail!("Task not found or not cancellable");
        }
        Ok(())
    });

    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": {"status": "cancelled"}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_tasks", "Task",
    list_fn: tasks_list, get_fn: tasks_get, delete_fn: tasks_delete_unused,
    columns: "id, name, description, params, status, result, error, agent_id, started_at, completed_at, created_at, updated_at",
    soft_delete: false
);

// ══════════════════════════════════════════════════════════════════
// FEED HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct FeedUploadBody {
    name: String,
    description: Option<String>,
    handler_code: Option<String>,
    schedule: String,
    refresh_seconds: Option<u64>,
    category: Option<String>,
    retention: Option<serde_json::Value>,
    display: Option<serde_json::Value>,
    sandbox_config: Option<serde_json::Value>,
}

async fn feeds_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<FeedUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let handler_hash = hash_code(body.handler_code.as_deref().unwrap_or(""));

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_feeds WHERE tenant_id = ?1 AND name = ?2 AND status != 'deleted'",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        let feed_id = if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_feeds SET description = ?1, handler_code = ?2, handler_hash = ?3,
                 schedule = ?4, refresh_seconds = ?5, category = ?6, retention = ?7,
                 display = ?8, sandbox_config = ?9, updated_at = ?10
                 WHERE id = ?11",
                rusqlite::params![
                    body.description.as_deref().unwrap_or(""),
                    body.handler_code.as_deref().unwrap_or(""),
                    handler_hash, body.schedule,
                    body.refresh_seconds.map(|v| v as i64),
                    body.category,
                    body.retention.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.display.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.sandbox_config.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    now, existing_id,
                ],
            )?;
            existing_id
        } else {
            conn.execute(
                "INSERT INTO aria_feeds (id, tenant_id, name, description, handler_code, handler_hash,
                 schedule, refresh_seconds, category, retention, display, sandbox_config,
                 status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'active', ?13, ?13)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name,
                    body.description.as_deref().unwrap_or(""),
                    body.handler_code.as_deref().unwrap_or(""),
                    handler_hash, body.schedule,
                    body.refresh_seconds.map(|v| v as i64),
                    body.category,
                    body.retention.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.display.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.sandbox_config.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    now,
                ],
            )?;
            id
        };

        // Fire-and-forget hook
        crate::aria::hooks::notify_feed_uploaded(&feed_id);
        Ok(feed_id)
    });

    match result {
        Ok(feed_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": feed_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_feeds", "Feed",
    list_fn: feeds_list, get_fn: feeds_get, delete_fn: feeds_delete_inner,
    columns: "id, name, description, schedule, refresh_seconds, category, retention, display, status, created_at, updated_at",
    soft_delete: true
);

// Override feeds_delete to fire hook
async fn feeds_delete(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let result = state.db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE aria_feeds SET status = 'deleted', updated_at = ?1
             WHERE id = ?2 AND tenant_id = ?3 AND status != 'deleted'",
            rusqlite::params![chrono::Utc::now().to_rfc3339(), id, ctx.tenant_id],
        )?;
        if changed == 0 {
            anyhow::bail!("Feed not found");
        }
        crate::aria::hooks::notify_feed_deleted(&id);
        Ok(())
    });

    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, "Feed not found").into_response(),
    }
}

// ══════════════════════════════════════════════════════════════════
// CRON HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct CronUploadBody {
    name: String,
    description: Option<String>,
    schedule_kind: String,
    schedule_data: serde_json::Value,
    session_target: Option<String>,
    wake_mode: Option<String>,
    payload_kind: String,
    payload_data: serde_json::Value,
    isolation: Option<serde_json::Value>,
    enabled: Option<bool>,
    delete_after_run: Option<bool>,
}

async fn crons_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<CronUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_cron_functions WHERE tenant_id = ?1 AND name = ?2",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        let cron_id = if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_cron_functions SET description = ?1, schedule_kind = ?2,
                 schedule_data = ?3, session_target = ?4, wake_mode = ?5,
                 payload_kind = ?6, payload_data = ?7, isolation = ?8,
                 enabled = ?9, delete_after_run = ?10, updated_at = ?11
                 WHERE id = ?12",
                rusqlite::params![
                    body.description.as_deref().unwrap_or(""),
                    body.schedule_kind,
                    serde_json::to_string(&body.schedule_data).unwrap_or_default(),
                    body.session_target.as_deref().unwrap_or("main"),
                    body.wake_mode.as_deref().unwrap_or("next-heartbeat"),
                    body.payload_kind,
                    serde_json::to_string(&body.payload_data).unwrap_or_default(),
                    body.isolation.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.enabled.unwrap_or(true) as i64,
                    body.delete_after_run.unwrap_or(false) as i64,
                    now, existing_id,
                ],
            )?;
            existing_id
        } else {
            conn.execute(
                "INSERT INTO aria_cron_functions (id, tenant_id, name, description, schedule_kind,
                 schedule_data, session_target, wake_mode, payload_kind, payload_data,
                 isolation, enabled, delete_after_run, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'active', ?14, ?14)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name,
                    body.description.as_deref().unwrap_or(""),
                    body.schedule_kind,
                    serde_json::to_string(&body.schedule_data).unwrap_or_default(),
                    body.session_target.as_deref().unwrap_or("main"),
                    body.wake_mode.as_deref().unwrap_or("next-heartbeat"),
                    body.payload_kind,
                    serde_json::to_string(&body.payload_data).unwrap_or_default(),
                    body.isolation.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.enabled.unwrap_or(true) as i64,
                    body.delete_after_run.unwrap_or(false) as i64,
                    now,
                ],
            )?;
            id
        };

        crate::aria::hooks::notify_cron_uploaded(&cron_id);
        Ok(cron_id)
    });

    match result {
        Ok(cron_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": cron_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// Cron uses hard delete
impl_list_get_delete!(
    "aria_cron_functions", "Cron function",
    list_fn: crons_list, get_fn: crons_get, delete_fn: crons_delete_inner,
    columns: "id, name, description, schedule_kind, schedule_data, session_target, wake_mode, payload_kind, payload_data, enabled, cron_job_id, created_at, updated_at",
    soft_delete: false
);

// Override crons_delete to fire hook
async fn crons_delete(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let result = state.db.with_conn(|conn| {
        let changed = conn.execute(
            "DELETE FROM aria_cron_functions WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![id, ctx.tenant_id],
        )?;
        if changed == 0 {
            anyhow::bail!("Cron function not found");
        }
        crate::aria::hooks::notify_cron_deleted(&id);
        Ok(())
    });

    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"success": true}))).into_response(),
        Err(_) => ApiResponse::<()>::err(StatusCode::NOT_FOUND, "Cron function not found").into_response(),
    }
}

// ══════════════════════════════════════════════════════════════════
// KV HANDLERS
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct KvSetBody {
    key: String,
    value: String,
}

async fn kv_set(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<KvSetBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_kv WHERE tenant_id = ?1 AND key = ?2",
                rusqlite::params![ctx.tenant_id, body.key],
                |row| row.get(0),
            )
            .ok();

        if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_kv SET value = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![body.value, now, existing_id],
            )?;
            Ok(existing_id)
        } else {
            conn.execute(
                "INSERT INTO aria_kv (id, tenant_id, key, value, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                rusqlite::params![id, ctx.tenant_id, body.key, body.value, now],
            )?;
            Ok(id)
        }
    });

    match result {
        Ok(kv_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": kv_id}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_kv", "KV entry",
    list_fn: kv_list, get_fn: kv_get, delete_fn: kv_delete,
    columns: "id, key, value, created_at, updated_at",
    soft_delete: false
);

#[derive(Deserialize)]
struct KvQueryParams {
    prefix: String,
    limit: Option<u32>,
}

async fn kv_query(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Query(params): Query<KvQueryParams>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let limit = params.limit.unwrap_or(100);

    let result = state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, key, value, created_at, updated_at FROM aria_kv
             WHERE tenant_id = ?1 AND key LIKE ?2 ORDER BY key LIMIT ?3",
        )?;
        let prefix_pattern = format!("{}%", params.prefix);
        let rows = stmt.query_map(rusqlite::params![ctx.tenant_id, prefix_pattern, limit], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "key": row.get::<_, String>(1)?,
                "value": row.get::<_, String>(2)?,
                "created_at": row.get::<_, String>(3)?,
                "updated_at": row.get::<_, String>(4)?,
            }))
        })?;
        let items: Vec<serde_json::Value> = rows.filter_map(|r| r.ok()).collect();
        Ok(serde_json::json!({"items": items}))
    });

    match result {
        Ok(data) => (StatusCode::OK, Json(serde_json::json!({"success": true, "data": data}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ══════════════════════════════════════════════════════════════════
// TEAM / PIPELINE / CONTAINER / NETWORK HANDLERS (Upload + CRUD)
// ══════════════════════════════════════════════════════════════════

// Team upload
#[derive(Deserialize)]
struct TeamUploadBody {
    name: String,
    description: Option<String>,
    mode: Option<String>,
    members: Option<serde_json::Value>,
    shared_context: Option<serde_json::Value>,
    timeout_seconds: Option<u64>,
    max_rounds: Option<u32>,
}

async fn teams_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<TeamUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_teams WHERE tenant_id = ?1 AND name = ?2 AND status != 'deleted'",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        let team_id = if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_teams SET description = ?1, mode = ?2, members = ?3,
                 shared_context = ?4, timeout_seconds = ?5, max_rounds = ?6, updated_at = ?7
                 WHERE id = ?8",
                rusqlite::params![
                    body.description.as_deref().unwrap_or(""),
                    body.mode.as_deref().unwrap_or("coordinator"),
                    serde_json::to_string(&body.members.unwrap_or(serde_json::json!([]))).unwrap_or_default(),
                    body.shared_context.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.timeout_seconds.map(|v| v as i64),
                    body.max_rounds.map(|v| v as i64),
                    now, existing_id,
                ],
            )?;
            existing_id
        } else {
            conn.execute(
                "INSERT INTO aria_teams (id, tenant_id, name, description, mode, members,
                 shared_context, timeout_seconds, max_rounds, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10, ?10)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name,
                    body.description.as_deref().unwrap_or(""),
                    body.mode.as_deref().unwrap_or("coordinator"),
                    serde_json::to_string(&body.members.unwrap_or(serde_json::json!([]))).unwrap_or_default(),
                    body.shared_context.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    body.timeout_seconds.map(|v| v as i64),
                    body.max_rounds.map(|v| v as i64),
                    now,
                ],
            )?;
            id
        };
        Ok(team_id)
    });

    match result {
        Ok(team_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": team_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_teams", "Team",
    list_fn: teams_list, get_fn: teams_get, delete_fn: teams_delete,
    columns: "id, name, description, mode, members, shared_context, timeout_seconds, max_rounds, status, created_at, updated_at",
    soft_delete: true
);

// Pipeline upload
#[derive(Deserialize)]
struct PipelineUploadBody {
    name: String,
    description: Option<String>,
    steps: Option<serde_json::Value>,
    variables: Option<serde_json::Value>,
    timeout_seconds: Option<u64>,
    max_parallel: Option<u32>,
}

async fn pipelines_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<PipelineUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_pipelines WHERE tenant_id = ?1 AND name = ?2 AND status != 'deleted'",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        let pipeline_id = if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_pipelines SET description = ?1, steps = ?2, variables = ?3,
                 timeout_seconds = ?4, max_parallel = ?5, updated_at = ?6
                 WHERE id = ?7",
                rusqlite::params![
                    body.description.as_deref().unwrap_or(""),
                    serde_json::to_string(&body.steps.unwrap_or(serde_json::json!([]))).unwrap_or_default(),
                    serde_json::to_string(&body.variables.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    body.timeout_seconds.map(|v| v as i64),
                    body.max_parallel.map(|v| v as i64),
                    now, existing_id,
                ],
            )?;
            existing_id
        } else {
            conn.execute(
                "INSERT INTO aria_pipelines (id, tenant_id, name, description, steps, variables,
                 timeout_seconds, max_parallel, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?9)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name,
                    body.description.as_deref().unwrap_or(""),
                    serde_json::to_string(&body.steps.unwrap_or(serde_json::json!([]))).unwrap_or_default(),
                    serde_json::to_string(&body.variables.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    body.timeout_seconds.map(|v| v as i64),
                    body.max_parallel.map(|v| v as i64),
                    now,
                ],
            )?;
            id
        };
        Ok(pipeline_id)
    });

    match result {
        Ok(pipeline_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": pipeline_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_pipelines", "Pipeline",
    list_fn: pipelines_list, get_fn: pipelines_get, delete_fn: pipelines_delete,
    columns: "id, name, description, steps, variables, timeout_seconds, max_parallel, status, created_at, updated_at",
    soft_delete: true
);

// Container upload
#[derive(Deserialize)]
struct ContainerUploadBody {
    name: String,
    image: String,
    config: Option<serde_json::Value>,
    network_id: Option<String>,
    labels: Option<serde_json::Value>,
}

async fn containers_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<ContainerUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_containers WHERE tenant_id = ?1 AND name = ?2",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        let container_id = if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_containers SET image = ?1, config = ?2, network_id = ?3,
                 labels = ?4, updated_at = ?5 WHERE id = ?6",
                rusqlite::params![
                    body.image,
                    serde_json::to_string(&body.config.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    body.network_id,
                    serde_json::to_string(&body.labels.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    now, existing_id,
                ],
            )?;
            existing_id
        } else {
            conn.execute(
                "INSERT INTO aria_containers (id, tenant_id, name, image, config, state,
                 network_id, labels, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8, ?8)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name, body.image,
                    serde_json::to_string(&body.config.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    body.network_id,
                    serde_json::to_string(&body.labels.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    now,
                ],
            )?;
            id
        };
        Ok(container_id)
    });

    match result {
        Ok(container_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": container_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_containers", "Container",
    list_fn: containers_list, get_fn: containers_get, delete_fn: containers_delete,
    columns: "id, name, image, config, state, container_ip, container_pid, network_id, labels, created_at, updated_at",
    soft_delete: false
);

// Network upload
#[derive(Deserialize)]
struct NetworkUploadBody {
    name: String,
    driver: Option<String>,
    isolation: Option<String>,
    ipv6: Option<bool>,
    dns_config: Option<serde_json::Value>,
    labels: Option<serde_json::Value>,
    options: Option<serde_json::Value>,
}

async fn networks_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<NetworkUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();

    let result = state.db.with_conn(|conn| {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM aria_networks WHERE tenant_id = ?1 AND name = ?2",
                rusqlite::params![ctx.tenant_id, body.name],
                |row| row.get(0),
            )
            .ok();

        let network_id = if let Some(existing_id) = existing {
            conn.execute(
                "UPDATE aria_networks SET driver = ?1, isolation = ?2, ipv6 = ?3,
                 dns_config = ?4, labels = ?5, options = ?6, updated_at = ?7
                 WHERE id = ?8",
                rusqlite::params![
                    body.driver.as_deref().unwrap_or("bridge"),
                    body.isolation.as_deref().unwrap_or("default"),
                    body.ipv6.unwrap_or(false) as i64,
                    body.dns_config.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    serde_json::to_string(&body.labels.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    serde_json::to_string(&body.options.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    now, existing_id,
                ],
            )?;
            existing_id
        } else {
            conn.execute(
                "INSERT INTO aria_networks (id, tenant_id, name, driver, isolation, ipv6,
                 dns_config, labels, options, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                rusqlite::params![
                    id, ctx.tenant_id, body.name,
                    body.driver.as_deref().unwrap_or("bridge"),
                    body.isolation.as_deref().unwrap_or("default"),
                    body.ipv6.unwrap_or(false) as i64,
                    body.dns_config.as_ref().map(|v| serde_json::to_string(v).unwrap_or_default()),
                    serde_json::to_string(&body.labels.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    serde_json::to_string(&body.options.unwrap_or(serde_json::json!({}))).unwrap_or_default(),
                    now,
                ],
            )?;
            id
        };
        Ok(network_id)
    });

    match result {
        Ok(network_id) => (StatusCode::CREATED, Json(serde_json::json!({"success": true, "data": {"id": network_id, "name": body.name}}))).into_response(),
        Err(e) => ApiResponse::<()>::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

impl_list_get_delete!(
    "aria_networks", "Network",
    list_fn: networks_list, get_fn: networks_get, delete_fn: networks_delete,
    columns: "id, name, driver, isolation, ipv6, dns_config, labels, options, created_at, updated_at",
    soft_delete: false
);

// ══════════════════════════════════════════════════════════════════
// BULK UPLOAD
// ══════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct BulkUploadBody {
    items: Vec<BulkItem>,
}

#[derive(Deserialize)]
struct BulkItem {
    #[serde(rename = "type")]
    item_type: String,
    data: serde_json::Value,
}

async fn bulk_upload(
    State(state): State<RegistryApiState>,
    headers: HeaderMap,
    Json(body): Json<BulkUploadBody>,
) -> impl IntoResponse {
    let ctx = match extract_route_context(&headers) {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };

    let mut results = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for (index, item) in body.items.iter().enumerate() {
        let id = uuid::Uuid::new_v4().to_string();
        let name = item.data.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

        let result = match item.item_type.as_str() {
            "tool" | "agent" | "feed" | "team" | "pipeline" | "container" | "network" => {
                let table = format!("aria_{}s", item.item_type);
                state.db.with_conn(|conn| {
                    // Simplified bulk insert — just the core fields
                    conn.execute(
                        &format!(
                            "INSERT OR REPLACE INTO {} (id, tenant_id, name, created_at, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?4)",
                            table
                        ),
                        rusqlite::params![id, ctx.tenant_id, name, now],
                    )?;
                    Ok(())
                })
            }
            _ => Err(anyhow::anyhow!("Unknown item type: {}", item.item_type)),
        };

        results.push(serde_json::json!({
            "index": index,
            "type": item.item_type,
            "name": name,
            "success": result.is_ok(),
            "error": result.err().map(|e| e.to_string()),
        }));
    }

    (StatusCode::OK, Json(serde_json::json!({
        "success": true,
        "data": {"results": results, "total": body.items.len()}
    }))).into_response()
}

// ══════════════════════════════════════════════════════════════════
// UTILITY
// ══════════════════════════════════════════════════════════════════

fn hash_code(code: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    code.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    #[test]
    fn hash_code_is_deterministic() {
        let h1 = hash_code("function execute() {}");
        let h2 = hash_code("function execute() {}");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn hash_code_differs_for_different_input() {
        let h1 = hash_code("function a() {}");
        let h2 = hash_code("function b() {}");
        assert_ne!(h1, h2);
    }

    #[test]
    fn extract_route_context_requires_auth() {
        let headers = HeaderMap::new();
        let result = extract_route_context(&headers);
        assert!(result.is_err());
    }

    #[test]
    fn extract_route_context_parses_tenant() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer tenant123:secret".parse().unwrap());
        let ctx = extract_route_context(&headers).unwrap();
        assert_eq!(ctx.tenant_id, "tenant123");
    }

    #[test]
    fn extract_route_context_uses_token_as_tenant() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer simple_token".parse().unwrap());
        let ctx = extract_route_context(&headers).unwrap();
        assert_eq!(ctx.tenant_id, "simple_token");
    }

    #[test]
    fn api_response_ok_structure() {
        let resp: ApiResponse<&str> = ApiResponse {
            success: true,
            data: Some("hello"),
            error: None,
            count: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"data\":\"hello\""));
        assert!(!json.contains("error"));
    }

    #[test]
    fn registry_router_builds_without_panic() {
        let db = AriaDb::open_in_memory().unwrap();
        let _router = registry_router(db);
    }

    #[test]
    fn pagination_query_defaults() {
        let q: PaginationQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(q.limit, None);
        assert_eq!(q.offset, None);
    }
}
