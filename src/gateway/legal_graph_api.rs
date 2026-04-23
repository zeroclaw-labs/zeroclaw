//! HTTP endpoints for Path B of the legal graph feature:
//!
//!   GET /api/legal/graph/subgraph?node=<slug>&depth=<N>&kinds=statute,case
//!       → graphify-compatible JSON `{nodes, edges, truncated}` subgraph.
//!   GET /api/legal/graph/path?from=<slug>&to=<slug>&max_depth=<N>
//!       → `{path: ["slug", ...] | null}`.
//!   GET /api/legal/graph/stats
//!       → summary counts.
//!   GET /legal-graph/viewer
//!       → embedded Cytoscape.js viewer HTML (no build step).
//!
//! All endpoints are read-only and open a fresh SQLite connection per
//! request (WAL mode handles concurrent readers).

use super::AppState;
use crate::vault::legal::graph_query;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/legal/graph/subgraph", get(handle_subgraph))
        .route("/api/legal/graph/path", get(handle_path))
        .route("/api/legal/graph/stats", get(handle_stats))
        .route("/legal-graph/viewer", get(handle_viewer))
}

// ───────── /api/legal/graph/subgraph ─────────

#[derive(Debug, Deserialize)]
pub struct SubgraphQuery {
    pub node: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// Comma-separated kinds filter, e.g. `statute,case` (empty = both).
    #[serde(default)]
    pub kinds: Option<String>,
}

fn default_depth() -> u32 {
    1
}

async fn handle_subgraph(
    State(state): State<AppState>,
    Query(q): Query<SubgraphQuery>,
) -> impl IntoResponse {
    let depth = q.depth.clamp(1, 3) as usize;
    let kinds = parse_kinds(q.kinds.as_deref());
    let workspace = state.config.lock().workspace_dir.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = open_read_only(&workspace)?;
        graph_query::neighbors(&conn, &q.node, depth, &kinds)
    })
    .await;

    match result {
        Ok(Ok(sg)) => (StatusCode::OK, Json(serde_json::to_value(&sg).unwrap())).into_response(),
        Ok(Err(e)) => http_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ───────── /api/legal/graph/path ─────────

#[derive(Debug, Deserialize)]
pub struct PathQuery {
    pub from: String,
    pub to: String,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

fn default_max_depth() -> u32 {
    4
}

async fn handle_path(
    State(state): State<AppState>,
    Query(q): Query<PathQuery>,
) -> impl IntoResponse {
    let max_depth = q.max_depth.clamp(1, 6) as usize;
    let workspace = state.config.lock().workspace_dir.clone();
    let res = tokio::task::spawn_blocking(move || {
        let conn = open_read_only(&workspace)?;
        graph_query::shortest_path(&conn, &q.from, &q.to, max_depth)
    })
    .await;

    match res {
        Ok(Ok(path)) => (StatusCode::OK, Json(json!({ "path": path }))).into_response(),
        Ok(Err(e)) => http_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ───────── /api/legal/graph/stats ─────────

async fn handle_stats(State(state): State<AppState>) -> impl IntoResponse {
    let workspace = state.config.lock().workspace_dir.clone();
    let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let conn = open_read_only(&workspace)?;
        let statutes: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vault_documents WHERE doc_type='statute_article'",
            [],
            |r| r.get(0),
        )?;
        let supplements: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vault_documents WHERE doc_type='statute_supplement'",
            [],
            |r| r.get(0),
        )?;
        let cases: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vault_documents WHERE doc_type='case'",
            [],
            |r| r.get(0),
        )?;
        let laws: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT value) FROM vault_frontmatter WHERE key='law_name'",
            [],
            |r| r.get(0),
        )?;
        let edges: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vault_links vl
               JOIN vault_documents d ON d.id = vl.source_doc_id
              WHERE d.doc_type IN ('statute_article','statute_supplement','case')",
            [],
            |r| r.get(0),
        )?;
        let resolved: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vault_links vl
               JOIN vault_documents d ON d.id = vl.source_doc_id
              WHERE d.doc_type IN ('statute_article','statute_supplement','case') AND vl.is_resolved = 1",
            [],
            |r| r.get(0),
        )?;
        Ok(json!({
            "statute_articles": statutes,
            "statute_supplements": supplements,
            "distinct_laws": laws,
            "cases": cases,
            "edges": edges,
            "edges_resolved": resolved,
            "edges_unresolved": edges - resolved,
        }))
    })
    .await;

    match res {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(e)) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

// ───────── /legal-graph/viewer ─────────

async fn handle_viewer() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        VIEWER_HTML,
    )
}

// ───────── Helpers ─────────

fn open_read_only(workspace_dir: &PathBuf) -> anyhow::Result<Connection> {
    let db_path = workspace_dir.join("memory").join("brain.db");
    if !db_path.exists() {
        anyhow::bail!(
            "brain.db not found at {} — run `zeroclaw vault legal ingest <dir>` first",
            db_path.display()
        );
    }
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='vault_documents'",
        [],
        |_| Ok(()),
    )?;
    Ok(conn)
}

fn parse_kinds(raw: Option<&str>) -> Vec<graph_query::NodeKind> {
    let Some(raw) = raw else {
        return vec![];
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match s {
            "statute" => Some(graph_query::NodeKind::Statute),
            "case" => Some(graph_query::NodeKind::Case),
            _ => None,
        })
        .collect()
}

fn http_error(status: StatusCode, msg: &str) -> axum::response::Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

// Embedded viewer HTML — single file, CDN-loaded Cytoscape.js. Kept in Rust
// source (not under web/) to avoid a separate build step for one page. If
// this grows past ~500 lines, move to a dedicated folder + rust-embed.
const VIEWER_HTML: &str = include_str!("legal_graph_viewer.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kinds_handles_common_inputs() {
        use graph_query::NodeKind::*;
        assert_eq!(parse_kinds(None), vec![] as Vec<graph_query::NodeKind>);
        assert_eq!(parse_kinds(Some("")), vec![] as Vec<graph_query::NodeKind>);
        assert_eq!(parse_kinds(Some("statute")), vec![Statute]);
        assert_eq!(parse_kinds(Some("case,statute")), vec![Case, Statute]);
        // Unknown values silently dropped.
        assert_eq!(parse_kinds(Some("garbage,case")), vec![Case]);
    }
}
