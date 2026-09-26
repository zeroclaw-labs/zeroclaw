//! Golden parity between the dashboard's HTTP routes and the core RPC
//! methods that replace them. Each test drives the real HTTP handler and the
//! RPC handler against identical state and requires the same body, so a route
//! can move to RPC without its clients seeing a change.
//!
//! The RPC side calls the method's handler after authorization. Routing and
//! the `Method::authz()` and selector checks are covered by the dispatcher's
//! own tests in the runtime crate.

use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use zeroclaw_api::jsonrpc::{
    FsDeleteRequest, FsMkdirRequest, FsMoveRequest, FsReadRequest, FsRmdirRequest,
    WorkspaceListRequest,
};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::rpc::workspace as rpc_ws;

use crate::api::tests::test_state;
use crate::api_browse::{
    BrowsePathBody, BrowseQuery, MoveBody, handle_agent_workspace_delete,
    handle_agent_workspace_list, handle_agent_workspace_mkdir, handle_agent_workspace_move,
    handle_agent_workspace_read, handle_browse, handle_browse_mkdir, handle_browse_rmdir,
};

const AGENT: &str = "alpha";

async fn body_json(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// An install tree with a shared area and one agent workspace holding a text
/// file, a binary file and a subdirectory.
fn install() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("shared/skills/one")).unwrap();
    std::fs::write(dir.path().join("shared/readme.txt"), b"hi").unwrap();
    let workspace = dir.path().join("agents").join(AGENT).join("workspace");
    std::fs::create_dir_all(workspace.join("notes")).unwrap();
    std::fs::write(workspace.join("notes/todo.md"), b"# todo\n").unwrap();
    std::fs::write(workspace.join("blob.bin"), [0u8, 159, 146, 150, 255]).unwrap();
    let config = Config {
        config_path: dir.path().join("config.toml"),
        ..Config::default()
    };
    (dir, config)
}

fn path_query(path: &str) -> Query<BrowseQuery> {
    Query(BrowseQuery {
        path: Some(path.to_string()),
    })
}

/// The RPC error, shaped like the HTTP adapter's error body so the two
/// messages compare directly.
fn rpc_error_body(err: &zeroclaw_api::jsonrpc::JsonRpcError) -> Value {
    serde_json::json!({ "error": err.message })
}

#[tokio::test]
async fn workspace_list_matches_the_agent_workspace_and_browse_routes() {
    let (_dir, config) = install();
    let state = test_state(config.clone());

    for path in ["", "notes"] {
        let (status, http) = body_json(
            handle_agent_workspace_list(
                State(state.clone()),
                HeaderMap::new(),
                Path(AGENT.to_string()),
                path_query(path),
            )
            .await,
        )
        .await;
        assert_eq!(status, 200, "{http}");
        let rpc = rpc_ws::handle_workspace_list(
            &config,
            &WorkspaceListRequest {
                agent: Some(AGENT.into()),
                path: Some(path.into()),
            },
        )
        .unwrap();
        assert_eq!(rpc, http, "agent workspace listing at {path:?}");
    }

    let (status, http) =
        body_json(handle_browse(State(state.clone()), HeaderMap::new(), path_query("")).await)
            .await;
    assert_eq!(status, 200, "{http}");
    let rpc = rpc_ws::handle_workspace_list(
        &config,
        &WorkspaceListRequest {
            agent: None,
            path: None,
        },
    )
    .unwrap();
    assert_eq!(rpc, http, "shared-area listing");
}

#[tokio::test]
async fn fs_read_matches_the_workspace_read_route_for_text_and_binary() {
    let (_dir, config) = install();
    let state = test_state(config.clone());
    for path in ["notes/todo.md", "blob.bin"] {
        let (status, http) = body_json(
            handle_agent_workspace_read(
                State(state.clone()),
                HeaderMap::new(),
                Path(AGENT.to_string()),
                path_query(path),
            )
            .await,
        )
        .await;
        assert_eq!(status, 200, "{http}");
        let rpc = rpc_ws::handle_fs_read(
            &config,
            &FsReadRequest {
                agent: AGENT.into(),
                path: path.into(),
            },
        )
        .unwrap();
        assert_eq!(rpc, http, "read of {path}");
    }
}

#[tokio::test]
async fn fs_errors_carry_the_same_message_as_the_routes() {
    let (_dir, config) = install();
    let state = test_state(config.clone());
    for path in ["missing.md", "../../etc/passwd", "notes"] {
        let (status, http) = body_json(
            handle_agent_workspace_read(
                State(state.clone()),
                HeaderMap::new(),
                Path(AGENT.to_string()),
                path_query(path),
            )
            .await,
        )
        .await;
        assert_ne!(status, 200, "{path} must fail over HTTP: {http}");
        let err = rpc_ws::handle_fs_read(
            &config,
            &FsReadRequest {
                agent: AGENT.into(),
                path: path.into(),
            },
        )
        .unwrap_err();
        assert_eq!(rpc_error_body(&err), http, "error for {path}");
    }
}

#[tokio::test]
async fn fs_mutations_match_their_routes_and_leave_the_same_tree() {
    // Each side mutates its own identical install, then the trees are listed
    // and compared, so a body can't match while the effect differs.
    let (_http_dir, http_config) = install();
    let (_rpc_dir, rpc_config) = install();
    let state = test_state(http_config.clone());
    let list = |config: &Config, agent: Option<&str>| {
        rpc_ws::handle_workspace_list(
            config,
            &WorkspaceListRequest {
                agent: agent.map(str::to_string),
                path: None,
            },
        )
        .unwrap()
    };

    // mkdir in the agent workspace and in the shared area.
    let (_, http) = body_json(
        handle_agent_workspace_mkdir(
            State(state.clone()),
            HeaderMap::new(),
            Path(AGENT.to_string()),
            Json(BrowsePathBody {
                path: "drafts".into(),
            }),
        )
        .await,
    )
    .await;
    let rpc = rpc_ws::handle_fs_mkdir(
        &rpc_config,
        &FsMkdirRequest {
            agent: Some(AGENT.into()),
            path: "drafts".into(),
        },
    )
    .unwrap();
    assert_eq!(rpc, http, "agent mkdir");

    let (_, http) = body_json(
        handle_browse_mkdir(
            State(state.clone()),
            HeaderMap::new(),
            Json(BrowsePathBody {
                path: "scratch".into(),
            }),
        )
        .await,
    )
    .await;
    let rpc = rpc_ws::handle_fs_mkdir(
        &rpc_config,
        &FsMkdirRequest {
            agent: None,
            path: "scratch".into(),
        },
    )
    .unwrap();
    assert_eq!(rpc, http, "shared mkdir");

    // move, then delete, in the agent workspace.
    let (_, http) = body_json(
        handle_agent_workspace_move(
            State(state.clone()),
            HeaderMap::new(),
            Path(AGENT.to_string()),
            Json(MoveBody {
                from: "notes/todo.md".into(),
                to: "drafts/todo.md".into(),
            }),
        )
        .await,
    )
    .await;
    let rpc = rpc_ws::handle_fs_move(
        &rpc_config,
        &FsMoveRequest {
            agent: AGENT.into(),
            from: "notes/todo.md".into(),
            to: "drafts/todo.md".into(),
        },
    )
    .unwrap();
    assert_eq!(rpc, http, "move");

    let (_, http) = body_json(
        handle_agent_workspace_delete(
            State(state.clone()),
            HeaderMap::new(),
            Path(AGENT.to_string()),
            Json(BrowsePathBody {
                path: "blob.bin".into(),
            }),
        )
        .await,
    )
    .await;
    let rpc = rpc_ws::handle_fs_delete(
        &rpc_config,
        &FsDeleteRequest {
            agent: AGENT.into(),
            path: "blob.bin".into(),
        },
    )
    .unwrap();
    assert_eq!(rpc, http, "delete");

    // rmdir in the shared area.
    let (_, http) = body_json(
        handle_browse_rmdir(
            State(state.clone()),
            HeaderMap::new(),
            Json(BrowsePathBody {
                path: "scratch".into(),
            }),
        )
        .await,
    )
    .await;
    let rpc = rpc_ws::handle_fs_rmdir(
        &rpc_config,
        &FsRmdirRequest {
            path: "scratch".into(),
        },
    )
    .unwrap();
    assert_eq!(rpc, http, "shared rmdir");

    assert_eq!(
        list(&rpc_config, Some(AGENT)),
        list(&http_config, Some(AGENT)),
        "the agent workspace ends up the same"
    );
    assert_eq!(
        list(&rpc_config, None),
        list(&http_config, None),
        "the shared area ends up the same"
    );
}

#[tokio::test]
async fn an_escaping_agent_alias_is_refused_alike_on_both_surfaces() {
    let (_dir, config) = install();
    let state = test_state(config.clone());
    let (status, http) = body_json(
        handle_agent_workspace_list(
            State(state),
            HeaderMap::new(),
            Path("..".to_string()),
            path_query(""),
        )
        .await,
    )
    .await;
    assert_eq!(status, 400, "{http}");
    let err = rpc_ws::handle_workspace_list(
        &config,
        &WorkspaceListRequest {
            agent: Some("..".into()),
            path: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS);
    assert_eq!(rpc_error_body(&err), http);
}

// ── Catalogs: integrations, CLI tools, plugins, A2A identity ─────────────

#[tokio::test]
async fn integrations_list_matches_the_integrations_route() {
    let config = Config::default();
    let state = test_state(config.clone());
    let (status, http) = body_json(
        crate::api::handle_api_integrations(State(state), HeaderMap::new())
            .await
            .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    assert_eq!(
        zeroclaw_runtime::rpc::catalog::integrations_body(&config),
        http
    );
}

#[tokio::test]
async fn cli_discover_matches_the_cli_tools_route() {
    let state = test_state(Config::default());
    let (status, http) = body_json(
        crate::api::handle_api_cli_tools(State(state), HeaderMap::new())
            .await
            .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    // Both scan the same PATH in the same process, so the lists match.
    assert_eq!(zeroclaw_runtime::rpc::catalog::cli_tools_body().await, http);
}

#[cfg(feature = "plugins-wasm")]
#[tokio::test]
async fn plugins_list_matches_the_plugins_route() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.plugins_dir = dir.path().join("plugins").to_string_lossy().to_string();
    std::fs::create_dir_all(dir.path().join("plugins")).unwrap();
    let state = test_state(config.clone());
    let (status, http) = body_json(
        crate::api_plugins::plugin_routes::list_plugins(State(state), HeaderMap::new())
            .await
            .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    assert_eq!(zeroclaw_runtime::rpc::catalog::plugins_body(&config), http);
}

fn a2a_config(published: bool) -> Config {
    let mut config = Config::default();
    config.a2a.server.enabled = true;
    let mut agent = zeroclaw_config::schema::AliasedAgentConfig {
        enabled: true,
        ..Default::default()
    };
    agent.a2a.published = published;
    config.agents.insert(AGENT.into(), agent);
    config
}

async fn get_route(state: crate::AppState, uri: &str) -> (u16, Option<Value>) {
    use tower::ServiceExt;
    let response = crate::a2a::a2a_routes()
        .with_state(state)
        .oneshot(
            axum::http::Request::get(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).ok())
}

#[tokio::test]
async fn a2a_identity_matches_the_well_known_card_routes() {
    let config = a2a_config(true);
    let state = test_state(config.clone());

    let (status, http) =
        get_route(state.clone(), zeroclaw_runtime::a2a_card::CATALOG_CARD_PATH).await;
    assert_eq!(status, 200);
    let rpc = zeroclaw_runtime::rpc::catalog::a2a_identity(&config, None).unwrap();
    assert_eq!(Some(rpc), http, "catalog card");

    let (status, http) =
        get_route(state, &format!("/a2a/{AGENT}/.well-known/agent-card.json")).await;
    assert_eq!(status, 200);
    let rpc = zeroclaw_runtime::rpc::catalog::a2a_identity(&config, Some(AGENT)).unwrap();
    assert_eq!(Some(rpc), http, "per-alias card");
}

#[tokio::test]
async fn a2a_identity_refuses_where_the_routes_return_not_found() {
    // An unpublished agent: 404 over HTTP, an error over RPC.
    let config = a2a_config(false);
    let (status, _) = get_route(
        test_state(config.clone()),
        &format!("/a2a/{AGENT}/.well-known/agent-card.json"),
    )
    .await;
    assert_eq!(status, 404);
    assert!(zeroclaw_runtime::rpc::catalog::a2a_identity(&config, Some(AGENT)).is_err());

    // The A2A server disabled: every card route 404s, every RPC call errors.
    let mut disabled = a2a_config(true);
    disabled.a2a.server.enabled = false;
    let (status, _) = get_route(
        test_state(disabled.clone()),
        zeroclaw_runtime::a2a_card::CATALOG_CARD_PATH,
    )
    .await;
    assert_eq!(status, 404);
    assert!(zeroclaw_runtime::rpc::catalog::a2a_identity(&disabled, None).is_err());
}

// ── Canvas ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn canvas_methods_match_the_canvas_routes() {
    use zeroclaw_runtime::rpc::canvas as rpc_canvas;
    let state = test_state(Config::default());
    let store = state.canvas_store.clone();

    // render: success, then the two refusals, over both surfaces.
    let (status, http) = body_json(
        crate::canvas::handle_canvas_post(
            State(state.clone()),
            HeaderMap::new(),
            Path("board".to_string()),
            Json(crate::canvas::CanvasPostBody {
                content_type: Some("text".into()),
                content: "one".into(),
            }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 201, "{http}");
    assert_eq!(http["canvas_id"], "board");
    assert_eq!(http["frame"]["content"], "one");

    for (content_type, content, expected) in [
        (Some("eval"), "alert(1)".to_string(), 400),
        (
            None,
            "x".repeat(zeroclaw_runtime::tools::MAX_CONTENT_SIZE + 1),
            413,
        ),
    ] {
        let (status, http) = body_json(
            crate::canvas::handle_canvas_post(
                State(state.clone()),
                HeaderMap::new(),
                Path("board".to_string()),
                Json(crate::canvas::CanvasPostBody {
                    content_type: content_type.map(str::to_string),
                    content: content.clone(),
                }),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, expected, "{http}");
        let failure = rpc_canvas::render_body(&store, "board", content_type, &content).unwrap_err();
        assert_eq!(failure.http_status(), expected);
        assert_eq!(serde_json::json!({ "error": failure.message() }), http);
    }

    // list, get, history against the same store.
    let (_, http) = body_json(
        crate::canvas::handle_canvas_list(State(state.clone()), HeaderMap::new())
            .await
            .into_response(),
    )
    .await;
    assert_eq!(rpc_canvas::list_body(&store), http, "list");
    let (_, http) = body_json(
        crate::canvas::handle_canvas_get(
            State(state.clone()),
            HeaderMap::new(),
            Path("board".to_string()),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(rpc_canvas::get_body(&store, "board").unwrap(), http, "get");
    let (_, http) = body_json(
        crate::canvas::handle_canvas_history(
            State(state.clone()),
            HeaderMap::new(),
            Path("board".to_string()),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(rpc_canvas::history_body(&store, "board"), http, "history");

    // A missing canvas: 404 and the same message.
    let (status, http) = body_json(
        crate::canvas::handle_canvas_get(
            State(state.clone()),
            HeaderMap::new(),
            Path("absent".to_string()),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 404);
    let failure = rpc_canvas::get_body(&store, "absent").unwrap_err();
    assert_eq!(serde_json::json!({ "error": failure.message() }), http);

    // clear.
    let (_, http) = body_json(
        crate::canvas::handle_canvas_clear(
            State(state.clone()),
            HeaderMap::new(),
            Path("board".to_string()),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(
        http,
        serde_json::json!({"canvas_id": "board", "status": "cleared"})
    );
    assert_eq!(rpc_canvas::clear_body(&store, "board"), http, "clear");
}

// ── Tool listing ─────────────────────────────────────────────────────────

#[tokio::test]
async fn tools_list_body_matches_the_tools_route() {
    let mut config = Config::default();
    config.agents.insert(
        AGENT.into(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            ..Default::default()
        },
    );
    let mut state = test_state(config.clone());
    let deps = zeroclaw_runtime::tools::listing::ToolListingDeps {
        runtime: std::sync::Arc::new(zeroclaw_runtime::platform::NativeRuntime::new()),
        memory: std::sync::Arc::new(zeroclaw_memory::NoneMemory::new("none")),
        canvas_store: state.canvas_store.clone(),
        sop_engine: None,
        sop_audit: None,
    };
    let specs = zeroclaw_runtime::tools::listing::agent_tool_specs(&config, AGENT, &deps)
        .await
        .unwrap()
        .unwrap_or_default();
    state.tools_registry_by_agent = std::sync::Arc::new(std::collections::HashMap::from([(
        AGENT.to_string(),
        std::sync::Arc::new(specs.clone()),
    )]));
    let (status, http) = body_json(
        crate::api::handle_api_tools(
            State(state),
            HeaderMap::new(),
            Query(crate::api::ToolsQuery {
                agent: Some(AGENT.into()),
            }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    assert_eq!(zeroclaw_runtime::rpc::catalog::tools_body(&specs), http);
}

// ── Metrics ──────────────────────────────────────────────────────────────

async fn metrics_route_text(state: crate::AppState) -> String {
    use tower::ServiceExt;
    let response = axum::Router::new()
        .route("/metrics", axum::routing::get(crate::handle_metrics))
        .with_state(state)
        .oneshot(
            axum::http::Request::get("/metrics")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The gateway is set up the way production builds it: its observer comes
/// from `create_observer`, which hands out the process's shared Prometheus
/// registry, so the route and the RPC method scrape the same registry.
#[cfg(feature = "observability-prometheus")]
#[tokio::test]
async fn metrics_scrape_equals_the_metrics_route_with_prometheus() {
    let mut config = Config::default();
    config.observability.backend = zeroclaw_config::schema::ObservabilityBackend::Prometheus;
    let mut state = test_state(config.clone());
    state.observer = std::sync::Arc::from(zeroclaw_runtime::observability::create_observer(
        &config.observability,
    ));
    state
        .observer
        .record_event(&zeroclaw_runtime::observability::ObserverEvent::HeartbeatTick);
    let http = metrics_route_text(state).await;
    let rpc = zeroclaw_runtime::observability::prometheus_exposition(&config.observability);
    assert_eq!(rpc, http);
    assert!(http.contains("heartbeat"), "{http}");
}

#[tokio::test]
async fn metrics_scrape_equals_the_metrics_route_without_prometheus() {
    let config = Config::default();
    let state = test_state(config.clone());
    let http = metrics_route_text(state).await;
    assert_eq!(
        zeroclaw_runtime::observability::prometheus_exposition(&config.observability),
        http
    );
}

// ── Pairing ──────────────────────────────────────────────────────────────

/// The bearer token every pairing fixture has paired, so the device routes'
/// bearer check passes.
const PAIRED_BEARER: &str = "p6-parity-bearer";

/// A gateway state with pairing required, one paired token, a private device
/// registry and a real `config.toml` for token persistence to write.
fn pairing_state(dir: &tempfile::TempDir) -> crate::AppState {
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "").unwrap();
    let mut config = Config {
        data_dir,
        config_path,
        ..Config::default()
    };
    config.gateway.require_pairing = true;
    let mut state = test_state(config.clone());
    state.pairing = std::sync::Arc::new(zeroclaw_config::pairing::PairingGuard::new(
        true,
        &[PAIRED_BEARER.to_string()],
        config.gateway.pairing_code,
    ));
    // A private registry with its schema, never the process-wide instance.
    state.device_registry = Some(std::sync::Arc::new(
        zeroclaw_runtime::devices::DeviceRegistry::new(&config.data_dir),
    ));
    state
}

fn bearer_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {PAIRED_BEARER}").parse().unwrap(),
    );
    headers
}

async fn text_body(response: Response) -> (u16, String) {
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn pairing_list_and_revoke_match_the_device_routes() {
    let dir = tempfile::tempdir().unwrap();
    let state = pairing_state(&dir);
    let registry = state.device_registry.clone();

    let (status, http) = body_json(
        crate::api_pairing::list_devices(State(state.clone()), bearer_headers())
            .await
            .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    assert_eq!(
        zeroclaw_runtime::devices::list_devices_body(registry.as_deref()).unwrap(),
        http
    );

    let (status, http) = text_body(
        crate::api_pairing::revoke_device(
            State(state.clone()),
            bearer_headers(),
            Path("no-such-device".to_string()),
        )
        .await
        .into_response(),
    )
    .await;
    let failure = zeroclaw_runtime::devices::revoke_device(
        registry.as_deref(),
        &state.pairing,
        state.config.clone(),
        state.config_write_lock.clone(),
        "no-such-device",
    )
    .await
    .unwrap_err();
    assert_eq!(status, failure.http_status);
    assert_eq!(http, failure.message);
}

#[tokio::test]
async fn pairing_new_code_matches_the_admin_paircode_route() {
    let loopback = axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 9)));
    for rotate in [None, Some("all")] {
        // Each surface starts from its own identical state, so a revocation
        // by one cannot change what the other reports.
        let http_dir = tempfile::tempdir().unwrap();
        let rpc_dir = tempfile::tempdir().unwrap();
        let http_state = pairing_state(&http_dir);
        let rpc_state = pairing_state(&rpc_dir);

        let response = crate::handle_admin_paircode_new(
            State(http_state),
            loopback,
            Query(crate::AdminPaircodeQuery {
                rotate: rotate.map(str::to_string),
            }),
        )
        .await
        .map(IntoResponse::into_response)
        .unwrap_or_else(IntoResponse::into_response);
        let (status, mut http) = body_json(response).await;
        let (rpc_status, mut rpc) = zeroclaw_runtime::devices::new_pairing_code(
            rpc_state.device_registry.as_deref(),
            &rpc_state.pairing,
            rpc_state.config.clone(),
            rpc_state.config_write_lock.clone(),
            rotate,
        )
        .await;
        assert_eq!(status, 200, "{rotate:?}: {http}");
        assert_eq!(status, rpc_status, "{rotate:?}");
        // Each call mints its own code; everything else must match.
        assert!(http["pairing_code"].is_string() && rpc["pairing_code"].is_string());
        http["pairing_code"] = Value::Null;
        rpc["pairing_code"] = Value::Null;
        assert_eq!(rpc, http, "{rotate:?}");
    }
}

// ── Channels ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn channels_list_and_relink_match_the_channel_routes() {
    use zeroclaw_runtime::rpc::channels::ChannelControl;
    let config = Config::default();
    let state = test_state(config.clone());
    let control = zeroclaw_channels::control::ChannelsControl;

    let (status, http) = body_json(
        crate::api::handle_api_channels(State(state.clone()), HeaderMap::new())
            .await
            .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    assert_eq!(control.list(&config, &state.pairing), http);

    let (status, http) = body_json(
        crate::api::handle_api_channel_relink(
            State(state.clone()),
            Path("nosuch.channel".to_string()),
            HeaderMap::new(),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 404, "{http}");
    let err = control.relink(&config, "nosuch.channel").unwrap_err();
    assert_eq!(
        err.data,
        Some(http.clone()),
        "the RPC error carries the route's body"
    );
    assert_eq!(serde_json::json!(err.message), http["error"]);
}

#[tokio::test]
async fn channels_bind_refuses_alike_on_both_surfaces() {
    use zeroclaw_runtime::rpc::channels::ChannelControl;
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        config_path: dir.path().join("config.toml"),
        ..Config::default()
    };
    let state = test_state(config);
    let control = zeroclaw_channels::control::ChannelsControl;
    for (channel_type, alias) in [("smtp", "main"), ("telegram", "absent")] {
        let (status, http) = body_json(
            crate::api_config::handle_api_channel_bind(
                State(state.clone()),
                HeaderMap::new(),
                Json(crate::api_config::ChannelBindBody {
                    channel_type: channel_type.into(),
                    alias: alias.into(),
                    identity: "@alice".into(),
                }),
            )
            .await,
        )
        .await;
        assert_ne!(status, 200, "{channel_type}.{alias}: {http}");
        let err = control
            .bind(
                &state.config,
                &state.config_write_lock,
                channel_type,
                alias,
                "@alice",
            )
            .await
            .unwrap_err();
        assert!(
            http.to_string().contains(&err.message),
            "{channel_type}.{alias}: route {http} vs rpc {}",
            err.message
        );
    }
}

// ── System ───────────────────────────────────────────────────────────────
//
// Only refusals and status: a real upgrade would run `zeroclaw update` on the
// test binary.

#[tokio::test]
async fn system_upgrade_refuses_alike_when_self_upgrade_is_disabled() {
    let mut config = Config::default();
    config.gateway.allow_self_upgrade = false;
    let state = test_state(config);
    let (status, http) = body_json(
        crate::version::handle_version_upgrade(
            State(state),
            HeaderMap::new(),
            axum::body::Bytes::new(),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 403, "{http}");
    let refusal = zeroclaw_runtime::self_upgrade::start_upgrade(
        false,
        crate::version::UpgradeRequest::default(),
        None,
    )
    .unwrap_err();
    assert_eq!(refusal.http_status, 403);
    assert_eq!(serde_json::json!({ "error": refusal.message }), http);
}

#[tokio::test]
async fn system_upgrade_status_matches_the_status_route() {
    let state = test_state(Config::default());
    let (status, http) = body_json(
        crate::version::handle_version_upgrade_status(
            State(state),
            HeaderMap::new(),
            Query(crate::version::UpgradeStatusQuery { handoff_id: None }),
        )
        .await
        .into_response(),
    )
    .await;
    assert_eq!(status, 200, "{http}");
    // Both read the one process-wide upgrade slot at the same moment.
    let rpc = serde_json::to_value(zeroclaw_runtime::self_upgrade::upgrade_status(None).unwrap())
        .unwrap();
    assert_eq!(rpc, http);
}
