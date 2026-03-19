//! REST API endpoints for the graph knowledge base.
//!
//! Provides HTTP access to graph operations for the web dashboard and external clients.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};

use super::AppState;

// ── Request/Response types ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct GraphQueryRequest {
    pub query: String,
}

#[derive(Deserialize)]
pub struct GraphSearchParams {
    pub q: Option<String>,
    pub limit: Option<usize>,
    pub node_type: Option<String>,
}

#[derive(Deserialize)]
pub struct GraphHotParams {
    pub threshold: Option<f64>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct GraphStatsResponse {
    pub total_nodes: usize,
    pub backend: String,
    pub healthy: bool,
}

#[derive(Deserialize)]
pub struct AddConceptRequest {
    pub name: String,
    pub description: String,
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct AddRelationRequest {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: Option<String>,
    pub weight: Option<f64>,
}

// ── Handlers ─────────────────────────────────────────────────────────

/// GET /api/graph/stats — Graph statistics
pub async fn handle_graph_stats(State(state): State<AppState>) -> impl IntoResponse {
    let count: usize = state.mem.count().await.unwrap_or_default();
    let healthy = state.mem.health_check().await;

    Json(GraphStatsResponse {
        total_nodes: count,
        backend: state.mem.name().to_string(),
        healthy,
    })
}

/// GET /api/graph/search — Search graph nodes
pub async fn handle_graph_search(
    State(state): State<AppState>,
    Query(params): Query<GraphSearchParams>,
) -> impl IntoResponse {
    let query = params.q.unwrap_or_default();
    let limit = params.limit.unwrap_or(10).min(100);

    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Missing 'q' parameter"})),
        );
    }

    match state.mem.recall(&query, limit, None).await {
        Ok(entries) => {
            let results: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "key": e.key,
                        "content": e.content,
                        "category": e.category.to_string(),
                        "score": e.score,
                        "timestamp": e.timestamp,
                    })
                })
                .collect();

            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "results": results,
                    "count": results.len(),
                    "query": query,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// GET /api/graph/hot — List hot nodes
pub async fn handle_graph_hot(
    State(state): State<AppState>,
    Query(params): Query<GraphHotParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(20).min(100);

    // Use recall with empty query to get hot nodes (which the graph backend supplements)
    match state.mem.recall("", limit, None).await {
        Ok(entries) => {
            let threshold = params.threshold.unwrap_or(0.0);
            let filtered: Vec<serde_json::Value> = entries
                .iter()
                .filter(|e| e.score.unwrap_or(0.0) >= threshold)
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "key": e.key,
                        "content": e.content,
                        "heat": e.score,
                        "category": e.category.to_string(),
                    })
                })
                .collect();

            Json(serde_json::json!({
                "nodes": filtered,
                "count": filtered.len(),
                "threshold": threshold,
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

/// GET /api/graph/nodes — List all nodes
pub async fn handle_graph_nodes(State(state): State<AppState>) -> impl IntoResponse {
    match state.mem.list(None, None).await {
        Ok(entries) => {
            let nodes: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "key": e.key,
                        "content": e.content,
                        "category": e.category.to_string(),
                        "score": e.score,
                        "timestamp": e.timestamp,
                    })
                })
                .collect();

            Json(serde_json::json!({
                "nodes": nodes,
                "count": nodes.len(),
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

/// POST /api/graph/concept — Add a concept
pub async fn handle_graph_add_concept(
    State(state): State<AppState>,
    Json(req): Json<AddConceptRequest>,
) -> impl IntoResponse {
    let content = format!("{}: {}", req.name, req.description);
    let key = format!("concept_{}", req.name.to_lowercase().replace(' ', "_"));

    match state
        .mem
        .store(&key, &content, crate::memory::MemoryCategory::Core, None)
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "created",
                "key": key,
                "name": req.name,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/graph/relation — Add a relation
pub async fn handle_graph_add_relation(
    State(state): State<AppState>,
    Json(req): Json<AddRelationRequest>,
) -> impl IntoResponse {
    let relation_type = req.relation_type.unwrap_or_else(|| "related".into());
    let content = format!(
        "Relation: {} --[{}]--> {}",
        req.from_id, relation_type, req.to_id
    );
    let key = format!("rel_{}_{}_{}", req.from_id, relation_type, req.to_id);

    match state
        .mem
        .store(
            &key,
            &content,
            crate::memory::MemoryCategory::Custom("relation".into()),
            None,
        )
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "created",
                "from_id": req.from_id,
                "to_id": req.to_id,
                "relation_type": relation_type,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// DELETE /api/graph/node/:id — Delete a node
pub async fn handle_graph_delete_node(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.mem.forget(&id).await {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deleted", "id": id})),
        ),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Node not found", "id": id})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// GET /api/graph/node/:id — Get a specific node
pub async fn handle_graph_get_node(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.mem.get(&id).await {
        Ok(Some(entry)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "id": entry.id,
                "key": entry.key,
                "content": entry.content,
                "category": entry.category.to_string(),
                "score": entry.score,
                "timestamp": entry.timestamp,
            })),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Node not found", "id": id})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// GET /api/graph/budget — Budget status
pub async fn handle_graph_budget(State(state): State<AppState>) -> impl IntoResponse {
    let (daily_cost, total_tokens) = state
        .cost_tracker
        .as_ref()
        .and_then(|ct| ct.get_summary().ok())
        .map(|summary| (summary.daily_cost_usd, summary.total_tokens))
        .unwrap_or((0.0, 0));

    Json(serde_json::json!({
        "daily_cost_usd": daily_cost,
        "total_tokens": total_tokens,
        "backend": state.mem.name(),
    }))
}
