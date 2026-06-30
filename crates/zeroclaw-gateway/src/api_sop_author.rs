//! Read-only SOP authoring surface for the web node editor.
//!
//! `GET /api/sops` lists the on-disk SOPs; `GET /api/sops/:name/graph` returns
//! the inferred blueprint projection for one SOP. Both resolve the sops dir and
//! default execution mode from live config exactly as the local RPC dispatch
//! does, so the web surface and the TUI render the same projection. Reads are
//! gated by the standard `/api/*` bearer-token check, never the admin gate.

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
    let guard = match engine.lock() {
        Ok(g) => g,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "SOP engine lock poisoned" })),
            )
                .into_response();
        }
    };
    match guard.get_run(&run_id) {
        Some(run) => {
            let graph = zeroclaw_runtime::sop::SopGraph::from_sop(&sop);
            let overlay = zeroclaw_runtime::sop::RunOverlay::project(&graph, run);
            Json(overlay).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("run '{run_id}' not found") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn write_sop(sops_dir: &std::path::Path, name: &str) {
        let dir = sops_dir.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SOP.toml"),
            format!(
                "[sop]\nname = \"{name}\"\ndescription = \"d\"\nversion = \"1.0.0\"\n\
                 priority = \"high\"\nexecution_mode = \"auto\"\n\n[[triggers]]\ntype = \"manual\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("SOP.md"),
            "# T\n\n## Steps\n\n1. **One** — do.\n   - tools: shell\n\n2. **Two** — done.\n",
        )
        .unwrap();
    }

    fn state_with_sops(sops_dir: &std::path::Path) -> crate::AppState {
        let mut config = zeroclaw_config::schema::Config::default();
        config.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());
        crate::api::tests::test_state(config)
    }

    #[tokio::test]
    async fn list_route_returns_saved_sops() {
        let tmp = tempfile::tempdir().unwrap();
        write_sop(tmp.path(), "alpha");
        let state = state_with_sops(tmp.path());
        let router = axum::Router::new()
            .route("/api/sops", get(super::handle_sops_list))
            .with_state(state);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/sops")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let sops = body["sops"].as_array().expect("sops array");
        assert_eq!(sops.len(), 1);
        assert_eq!(sops[0]["name"], "alpha");
    }

    #[tokio::test]
    async fn graph_route_returns_projection() {
        let tmp = tempfile::tempdir().unwrap();
        write_sop(tmp.path(), "beta");
        let state = state_with_sops(tmp.path());
        let router = axum::Router::new()
            .route("/api/sops/{name}/graph", get(super::handle_sop_graph))
            .with_state(state);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/sops/beta/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let graph: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
        assert!(graph["wires"].is_array());
        assert!(graph["diagnostics"].is_array());
    }

    #[tokio::test]
    async fn graph_route_unknown_name_is_404() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_with_sops(tmp.path());
        let router = axum::Router::new()
            .route("/api/sops/{name}/graph", get(super::handle_sop_graph))
            .with_state(state);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/sops/missing/graph")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    fn engine_with_run(sops_dir: &std::path::Path, sop_name: &str) -> (crate::AppState, String) {
        use std::sync::{Arc, Mutex};
        let mut state = state_with_sops(sops_dir);
        let config = zeroclaw_config::schema::SopConfig {
            sops_dir: Some(sops_dir.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut engine = zeroclaw_runtime::sop::SopEngine::new(config);
        engine.reload(sops_dir);
        let event = zeroclaw_runtime::sop::SopEvent {
            source: zeroclaw_runtime::sop::SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "t".into(),
        };
        engine.start_run(sop_name, event).expect("run starts");
        let run_id = engine
            .active_runs()
            .keys()
            .next()
            .expect("one active run")
            .clone();
        state.sop_engine = Some(Arc::new(Mutex::new(engine)));
        (state, run_id)
    }

    #[tokio::test]
    async fn overlay_route_projects_run_onto_graph() {
        let tmp = tempfile::tempdir().unwrap();
        write_sop(tmp.path(), "gamma");
        let (state, run_id) = engine_with_run(tmp.path(), "gamma");
        let router = axum::Router::new()
            .route(
                "/api/sops/{name}/runs/{run_id}/overlay",
                get(super::handle_sop_run_overlay),
            )
            .with_state(state);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sops/gamma/runs/{run_id}/overlay"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let overlay: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(overlay["run_id"], run_id);
        assert!(overlay["status"] == "running" || overlay["status"] == "waiting_approval");
        let nodes = overlay["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["state"], "active");
        assert_eq!(nodes[1]["state"], "pending");
    }

    #[tokio::test]
    async fn overlay_route_unknown_run_is_404() {
        let tmp = tempfile::tempdir().unwrap();
        write_sop(tmp.path(), "gamma");
        let (state, _run_id) = engine_with_run(tmp.path(), "gamma");
        let router = axum::Router::new()
            .route(
                "/api/sops/{name}/runs/{run_id}/overlay",
                get(super::handle_sop_run_overlay),
            )
            .with_state(state);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/sops/gamma/runs/run-nope/overlay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn overlay_route_engine_disabled_is_503() {
        let tmp = tempfile::tempdir().unwrap();
        write_sop(tmp.path(), "gamma");
        let state = state_with_sops(tmp.path());
        let router = axum::Router::new()
            .route(
                "/api/sops/{name}/runs/{run_id}/overlay",
                get(super::handle_sop_run_overlay),
            )
            .with_state(state);
        let resp = router
            .oneshot(
                Request::builder()
                    .uri("/api/sops/gamma/runs/run-1/overlay")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
}
