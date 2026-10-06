use super::*;

use std::collections::BTreeSet;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

fn bootstrap(list: &[&str], socket_env: Option<&str>) -> Result<Bootstrap, String> {
    match parse_args(args(list), socket_env.map(str::to_owned))? {
        Invocation::Serve(bootstrap) => Ok(bootstrap),
        other => panic!("expected a serve invocation, got {other:?}"),
    }
}

// ── Bootstrap ────────────────────────────────────────────────────

#[test]
fn the_endpoint_comes_from_the_socket_flag_then_the_environment_then_the_data_dir() {
    let flag = bootstrap(
        &["--socket", "/run/flag.sock", "--data-dir", "/var/zc"],
        Some("/run/env.sock"),
    )
    .unwrap();
    assert_eq!(flag.endpoint, PathBuf::from("/run/flag.sock"));

    let env = bootstrap(&["--data-dir", "/var/zc"], Some("/run/env.sock")).unwrap();
    assert_eq!(env.endpoint, PathBuf::from("/run/env.sock"));

    let data_dir = bootstrap(&["--data-dir", "/var/zc"], Some("  ")).unwrap();
    assert_eq!(
        data_dir.endpoint,
        zeroclaw_rpc_client::endpoint::default_endpoint(Path::new("/var/zc"))
    );
}

#[test]
fn without_an_endpoint_it_refuses_and_never_falls_back_to_config() {
    let refused = bootstrap(&[], None).unwrap_err();
    assert!(
        refused.contains("--socket") && refused.contains("config.toml"),
        "{refused}"
    );
    for flag in ["--config", "--config-dir"] {
        let refused = bootstrap(&[flag, "/etc/zeroclaw"], None).unwrap_err();
        assert!(refused.contains("never reads config.toml"), "{refused}");
    }
}

#[test]
fn defaults_serve_on_loopback_without_tls_or_dashboard() {
    let plain = bootstrap(&["--socket", "/run/zc.sock"], None).unwrap();
    assert_eq!(plain.listen, DEFAULT_LISTEN.parse::<SocketAddr>().unwrap());
    assert_eq!(plain.web_dist, None);
    assert_eq!(plain.tls, None);
}

#[test]
fn a_public_listen_address_needs_an_explicit_opt_in() {
    let refused = bootstrap(&["--socket", "/s", "--listen", "0.0.0.0:8080"], None).unwrap_err();
    assert!(refused.contains("--allow-public-bind"), "{refused}");
    let allowed = bootstrap(
        &[
            "--socket",
            "/s",
            "--listen",
            "0.0.0.0:8080",
            "--allow-public-bind",
        ],
        None,
    )
    .unwrap();
    assert_eq!(allowed.listen.port(), 8080);
}

#[test]
fn tls_needs_both_files_and_malformed_arguments_are_refused() {
    let tls = bootstrap(
        &[
            "--socket",
            "/s",
            "--tls-cert",
            "c.pem",
            "--tls-key",
            "k.pem",
        ],
        None,
    )
    .unwrap();
    assert_eq!(
        tls.tls,
        Some(TlsFiles {
            cert: "c.pem".into(),
            key: "k.pem".into()
        })
    );
    for list in [
        &["--socket", "/s", "--tls-cert", "c.pem"][..],
        &["--socket", "/s", "--tls-key", "k.pem"][..],
        &["--socket"][..],
        &["--socket", "/s", "--listen", "not-an-address"][..],
        &["--socket", "/s", "--frobnicate"][..],
    ] {
        assert!(bootstrap(list, None).is_err(), "{list:?} must be refused");
    }
    assert_eq!(parse_args(args(&["--help"]), None), Ok(Invocation::Help));
    assert_eq!(parse_args(args(&["-V"]), None), Ok(Invocation::Version));
}

// ── The fail-closed route map ────────────────────────────────────

/// Routes of the in-process gateway the preview serves: its own, then the
/// dashboard routes the core serves.
const SERVED: &[&str] = &[
    "/health",
    "/api/openapi.json",
    "/api/docs",
    "/api/health",
    "/api/tuis",
    "/api/cost",
    "/api/events/history",
    "/api/sessions",
    "/api/sessions/{id}/messages",
    "/api/sessions/{id}/state",
    "/api/sessions/{id}",
];

/// Route paths the in-process gateway registers with a string literal, from
/// its production source (test modules excluded).
///
/// A source scan, not the router itself: it covers the four files that
/// register production routes today. A route registered elsewhere, or
/// through a `const`, `format!`, `route_service` or `nest`, is invisible to
/// it; add that file here, or list the path by hand, when one appears. The
/// other route-registering modules (`ws`, `sse`, `api_pairing`,
/// `api_config`, `webhook_ingress`) register routes only in their tests.
fn in_process_routes() -> BTreeSet<String> {
    let sources = [
        include_str!("../lib.rs"),
        include_str!("../api_oidc.rs"),
        include_str!("../a2a.rs"),
        include_str!("../plugin_webhook.rs"),
    ];
    let mut paths = BTreeSet::new();
    for source in sources {
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("split yields the head");
        let mut rest = production;
        while let Some(at) = rest.find(".route(") {
            rest = &rest[at + ".route(".len()..];
            let literal = rest.trim_start();
            if let Some(body) = literal.strip_prefix('"')
                && let Some(end) = body.find('"')
            {
                paths.insert(body[..end].to_owned());
            }
        }
    }
    paths
}

#[test]
fn every_in_process_route_is_served_or_refused() {
    let classified: BTreeSet<&str> = REFUSED
        .iter()
        .map(|(path, _, _)| *path)
        .chain(SERVED.iter().copied())
        .collect();
    let routes = in_process_routes();
    assert!(routes.len() > 100, "the scan found the router: {routes:?}");
    let unclassified: Vec<&String> = routes
        .iter()
        .filter(|path| !path.starts_with("/_app/") && !classified.contains(path.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "routes the preview neither serves nor refuses: {unclassified:?}"
    );

    // The table names no route the in-process gateway lacks, except the A2A
    // card paths, which the router builds from constants.
    let from_constants = [
        "/.well-known/agents-card.json",
        "/a2a/.well-known/agents-card.json",
        "/a2a/{alias}/.well-known/agent-card.json",
    ];
    let stale: Vec<&&str> = classified
        .iter()
        .filter(|path| !routes.contains(**path) && !from_constants.contains(path))
        .collect();
    assert!(
        stale.is_empty(),
        "the table names unknown routes: {stale:?}"
    );
}

#[test]
fn every_refused_route_names_known_methods() {
    for (path, methods, _) in REFUSED {
        assert!(!methods.is_empty(), "{path}");
        // Panics on a method the router cannot express.
        let _ = method_filter(methods);
    }
}

// ── Refusals the operator can act on ─────────────────────────────

async fn explained(error: CoreError) -> (StatusCode, serde_json::Value) {
    use http_body_util::BodyExt as _;
    let response = explain(error);
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn a_protocol_mismatch_says_to_install_matching_versions() {
    let (status, body) = explained(CoreError::Rpc(zeroclaw_api::jsonrpc::JsonRpcError {
        code: zeroclaw_api::jsonrpc::error_codes::VERSION_MISMATCH,
        message: "Protocol version mismatch: server=1, client=2".into(),
        data: None,
    }))
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "core_incompatible");
    assert_eq!(
        body["error"], "Protocol version mismatch: server=1, client=2",
        "the core's own words are kept"
    );
    assert!(
        body["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("matching versions")),
        "{body}"
    );
}

#[tokio::test]
async fn a_refused_credential_and_an_unreachable_core_explain_themselves_distinctly() {
    let (status, body) = explained(CoreError::AuthRequired("credential rejected".into())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "auth_required");
    assert!(
        body["hint"].as_str().is_some_and(|h| h.contains("Bearer")),
        "{body}"
    );

    let (status, body) = explained(CoreError::Unavailable("socket missing".into())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "core_unavailable");
    assert!(
        body["hint"]
            .as_str()
            .is_some_and(|h| h.contains("zeroclaw daemon")),
        "{body}"
    );
}

#[tokio::test]
async fn busy_timeout_and_an_untrusted_endpoint_get_their_own_hints() {
    let (status, body) = explained(CoreError::UntrustedEndpoint(
        "refusing to send the credential".into(),
    ))
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "core_untrusted_endpoint");
    assert!(
        body["hint"]
            .as_str()
            .is_some_and(|h| h.contains("same OS account")),
        "{body}"
    );

    let (_, busy) = explained(CoreError::Busy).await;
    let (_, timeout) = explained(CoreError::Timeout).await;
    assert_eq!(busy["code"], "core_busy");
    assert_eq!(timeout["code"], "core_timeout");
    for body in [&busy, &timeout] {
        assert!(
            !body["hint"]
                .as_str()
                .is_some_and(|h| h.contains("zeroclaw daemon")),
            "a running core is not told to start: {body}"
        );
    }
    assert_ne!(busy["hint"], timeout["hint"]);
}

/// The serving note comes from the Fluent catalog: a missing key would
/// render as the `{key}` sentinel instead of the text.
#[test]
fn the_serving_notice_resolves_via_fluent() {
    let notice = serving_notice("http://127.0.0.1:42617", Path::new("/run/zeroclaw.sock"));
    assert!(!notice.starts_with('{'), "missing Fluent string: {notice}");
    assert!(notice.contains("http://127.0.0.1:42617"), "{notice}");
    assert!(notice.contains("/run/zeroclaw.sock"), "{notice}");
}

// ── The router, against a real core on a real socket ─────────────

#[cfg(unix)]
mod against_a_core {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt as _;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt as _;

    const TOKEN: &str = "zc_preview_token";

    /// A core serving its real local listener on a socket under `dir`.
    struct Core {
        ctx: Arc<zeroclaw_runtime::rpc::context::RpcContext>,
        endpoint: PathBuf,
        cancel: CancellationToken,
        listener: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    impl Core {
        async fn start(dir: &Path) -> Self {
            Self::serve(Self::context(dir)).await
        }

        fn context(dir: &Path) -> Arc<zeroclaw_runtime::rpc::context::RpcContext> {
            Self::context_with(dir, |_| {})
        }

        /// [`Self::context`] with `adjust` applied to the core's config.
        fn context_with(
            dir: &Path,
            adjust: impl FnOnce(&mut zeroclaw_config::schema::Config),
        ) -> Arc<zeroclaw_runtime::rpc::context::RpcContext> {
            assert!(
                std::env::var_os("ZEROCLAW_SOCKET").is_none(),
                "ZEROCLAW_SOCKET must be unset for these tests"
            );
            let mut config = zeroclaw_config::schema::Config {
                data_dir: dir.to_path_buf(),
                config_path: dir.join("config.toml"),
                ..Default::default()
            };
            config.gateway.paired_tokens = vec![TOKEN.into()];
            adjust(&mut config);
            let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
                16,
                Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                    4, 10, 60,
                )),
            ));
            zeroclaw_runtime::rpc::context::RpcContext::for_live_test(config, sessions)
        }

        async fn serve(ctx: Arc<zeroclaw_runtime::rpc::context::RpcContext>) -> Self {
            let endpoint = zeroclaw_runtime::rpc::local::socket_path(&ctx.config.read());
            let cancel = CancellationToken::new();
            let listener = {
                let ctx = Arc::clone(&ctx);
                let cancel = cancel.clone();
                zeroclaw_spawn::spawn!(async move {
                    zeroclaw_runtime::rpc::local::run_local_listener(
                        ctx,
                        cancel,
                        Arc::new(AtomicUsize::new(0)),
                        None,
                    )
                    .await
                })
            };
            for _ in 0..250 {
                if tokio::net::UnixStream::connect(&endpoint).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Self {
                ctx,
                endpoint,
                cancel,
                listener,
            }
        }

        /// Stop the listener and hand back the context for a restart.
        async fn stop(self) -> Arc<zeroclaw_runtime::rpc::context::RpcContext> {
            self.cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(10), self.listener).await;
            self.ctx
        }
    }

    /// A dashboard build laid out as `vite build` writes it: the page at the
    /// root, assets under `assets/`, all referenced as `/_app/...`.
    fn web_dist(dir: &Path) -> PathBuf {
        let dist = dir.join("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(
            dist.join("index.html"),
            r#"<html>preview dashboard<script src="/_app/assets/app.js"></script></html>"#,
        )
        .unwrap();
        std::fs::write(dist.join("assets").join("app.js"), "console.log(1)").unwrap();
        dist
    }

    async fn get(router: &Router, path: &str, token: Option<&str>) -> (StatusCode, String) {
        send(router, "GET", path, token).await
    }

    async fn send(
        router: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn json_of(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON ({e}): {body}"))
    }

    async fn headers_of(router: &Router, path: &str) -> axum::http::HeaderMap {
        router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .headers()
            .clone()
    }

    /// A listener that is not a core: it records what each connection
    /// writes, up to the first newline or EOF, then closes it.
    struct Recorder {
        endpoint: PathBuf,
        seen: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Recorder {
        fn bind(dir: &Path) -> Self {
            use tokio::io::AsyncBufReadExt as _;
            let endpoint = dir.join("daemon.sock");
            let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let task = {
                let seen = Arc::clone(&seen);
                zeroclaw_spawn::spawn!(async move {
                    while let Ok((stream, _)) = listener.accept().await {
                        let mut reader = tokio::io::BufReader::new(stream);
                        let mut bytes = Vec::new();
                        let _ = tokio::time::timeout(
                            Duration::from_secs(5),
                            reader.read_until(b'\n', &mut bytes),
                        )
                        .await;
                        seen.lock().unwrap().push(bytes);
                    }
                })
            };
            Self {
                endpoint,
                seen,
                task,
            }
        }

        fn connections(&self) -> Vec<Vec<u8>> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Drop for Recorder {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn wait_for_connections(recorder: &Recorder, count: usize) {
        for _ in 0..250 {
            if recorder.connections().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "expected {count} connections, saw {}",
            recorder.connections().len()
        );
    }

    #[tokio::test]
    async fn the_preview_serves_through_the_core_and_refuses_everything_else() {
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(tmp.path()).await;
        let router = router(
            CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount),
            core.endpoint.clone(),
            Some(web_dist(tmp.path())),
        );

        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["core"]["link"], "reachable");

        // The core-link diagnostic needs the caller's own credential.
        let (status, body) = get(&router, CORE_LINK_PATH, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(json_of(&body)["code"], "auth_required");
        assert!(json_of(&body)["hint"].is_string(), "{body}");
        let (status, body) = get(&router, CORE_LINK_PATH, Some("zc_not_paired")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        let (status, body) = get(&router, CORE_LINK_PATH, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let link = json_of(&body);
        assert_eq!(link["principal_id"], "shared-operator");
        assert_eq!(link["core"]["server_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(link["gateway"]["protocol_version"], RPC_PROTOCOL_VERSION);

        // A dashboard route not served yet: the credential first, then a
        // refusal that names the route.
        let (status, body) = get(&router, "/api/cron", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        let (status, body) = get(&router, "/api/cron", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let refused = json_of(&body);
        assert_eq!(refused["code"], "capability_missing");
        assert_eq!(refused["route"], "GET /api/cron");
        assert_eq!(refused["deferred"], false);
        // A path the preview serves for some methods still refuses the rest.
        for (method, path, route) in [
            ("PUT", "/api/sessions/abc", "PUT /api/sessions/{id}"),
            (
                "POST",
                "/api/sessions/abc/messages",
                "POST /api/sessions/{id}/messages",
            ),
        ] {
            let (status, body) = send(&router, method, path, Some(TOKEN)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
            assert_eq!(json_of(&body)["route"], route);
        }

        // A deferred route carries no bearer by nature and is refused as is.
        for (method, path, route) in [
            ("POST", "/webhook", "POST /webhook"),
            ("POST", "/pair", "POST /pair"),
            ("GET", "/acp", "GET /acp"),
            ("GET", "/oidc/callback", "GET /oidc/callback"),
            ("POST", "/plugin/inbox", "POST /plugin/{path}"),
        ] {
            let (status, body) = send(&router, method, path, None).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
            let refused = json_of(&body);
            assert_eq!(refused["code"], "capability_missing", "{path}");
            assert_eq!(refused["route"], route);
            assert_eq!(refused["deferred"], true);
        }

        // Nothing under an API prefix falls through to the dashboard page.
        for path in [
            "/api/no-such-route",
            "/ws/other",
            "/admin/other",
            "/api",
            "/ws",
            "/acp/",
            "/pair/",
        ] {
            let (status, body) = get(&router, path, Some(TOKEN)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
            assert_eq!(json_of(&body)["code"], "not_found", "{path}");
        }
        // Dashboard pages and assets are served from the build.
        let (status, body) = get(&router, "/sessions", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("preview dashboard"), "{body}");
        // Assets resolve where the page's own references point.
        let (status, body) = get(&router, "/_app/assets/app.js", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "console.log(1)");
        let (status, _) = get(&router, "/api/openapi.json", None).await;
        assert_eq!(status, StatusCode::OK);

        // Core down: the health check and core-backed routes say so, with a
        // hint the dashboard can show; a bad credential is still a 401.
        let ctx = core.stop().await;
        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let health = json_of(&body);
        assert_eq!(health["code"], "core_unavailable");
        assert_eq!(health["core"]["link"], "unreachable");
        let (status, body) = get(&router, CORE_LINK_PATH, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(json_of(&body)["code"], "core_unavailable");
        assert!(
            json_of(&body)["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("zeroclaw daemon")),
            "{body}"
        );
        let (status, _) = get(&router, CORE_LINK_PATH, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // The core comes back on the same endpoint: the next request
        // reconnects with the caller's credential.
        let core = Core::serve(ctx).await;
        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = get(&router, CORE_LINK_PATH, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        core.stop().await;
    }

    /// The in-process gateway's body for `path` when the request reaches the
    /// core through `core`.
    async fn in_process_body(state: &crate::AppState, core: &CoreRpc, path: &str) -> String {
        use crate::api::{
            handle_api_cost, handle_api_health, handle_api_sessions_list, handle_api_tuis,
        };
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().unwrap(),
        );
        let access = core
            .access(&headers)
            .await
            .expect("served through the core");
        assert!(matches!(access, CoreAccess::Core(_)));
        let state = State(state.clone());
        let response = match path {
            "/api/health" => handle_api_health(state, headers, access)
                .await
                .into_response(),
            "/api/tuis" => handle_api_tuis(state, headers, access)
                .await
                .into_response(),
            "/api/cost" => handle_api_cost(state, headers, Query(CostQuery::default()), access)
                .await
                .into_response(),
            "/api/cost?agent=main" => {
                let query = CostQuery {
                    agent: Some("main".into()),
                    ..CostQuery::default()
                };
                handle_api_cost(state, headers, Query(query), access)
                    .await
                    .into_response()
            }
            "/api/events/history" => crate::sse::handle_events_history(state, headers, access)
                .await
                .into_response(),
            // Served from the gateway's own store even with a core attached.
            "/api/sessions" => handle_api_sessions_list(state, headers, access)
                .await
                .into_response(),
            other => panic!("no in-process handler for {other}"),
        };
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&body).into_owned()
    }

    /// Each ported dashboard route answers through the separate gateway
    /// exactly what the in-process gateway answers when the same request
    /// reaches the same core, and lists only terminals as terminals.
    #[tokio::test]
    async fn the_ported_routes_answer_as_the_in_process_gateway_does() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = Core::context(tmp.path());
        let config = ctx.config.read().clone();
        let backend = zeroclaw_infra::make_session_backend(
            &config.data_dir,
            &config.channels.session_backend,
        )
        .expect("open the session store");
        backend
            .append("gw_alpha", &zeroclaw_providers::ChatMessage::user("hi"))
            .unwrap();
        backend.set_session_agent_alias("gw_alpha", "main").unwrap();
        let history = Arc::new(crate::sse::EventBuffer::new(8));
        history.push(json!({ "type": "agent_start", "provider": "test" }));
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context has one owner");
            ctx.session_backend = Some(Arc::clone(&backend));
            ctx.event_history = Some(history);
        }
        let core = Core::serve(ctx).await;
        let rpc = CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount);
        let preview = router(rpc.clone(), core.endpoint.clone(), None);
        let state = crate::api::tests::test_state_with_session_backend(config, backend);

        // A terminal attached to the core next to the gateway.
        let terminal = RpcClient::connect_local(
            &core.endpoint,
            zeroclaw_rpc_client::ConnectOptions {
                auth_token: Some(TOKEN.into()),
                ..Default::default()
            },
        )
        .await
        .expect("a terminal connects");
        let terminal_id = terminal
            .handshake()
            .tui_id
            .clone()
            .expect("the core names the terminal");

        for path in [
            "/api/health",
            "/api/tuis",
            "/api/cost",
            "/api/cost?agent=main",
            "/api/events/history",
            "/api/sessions",
        ] {
            let (status, body) = get(&preview, path, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
            assert!(json_of(&body)["hint"].is_string(), "{path}: {body}");

            // The health snapshot is process-wide and its `updated_at` is the
            // moment it was taken, so it is compared without that, and again
            // if a parallel test touched a component in between.
            let comparable = |body: &str| {
                let mut body = json_of(body);
                if let Some(health) = body.get_mut("health").and_then(|h| h.as_object_mut()) {
                    health.remove("updated_at");
                }
                body
            };
            for attempt in 0.. {
                let (status, served) = get(&preview, path, Some(TOKEN)).await;
                assert_eq!(status, StatusCode::OK, "{path}: {served}");
                let in_process = in_process_body(&state, &rpc, path).await;
                if comparable(&served) == comparable(&in_process) {
                    break;
                }
                assert!(
                    attempt < 20,
                    "{path}: the preview answered {served}, the in-process gateway {in_process}"
                );
            }
        }

        let (_, body) = get(&preview, "/api/tuis", Some(TOKEN)).await;
        let listed: Vec<String> = json_of(&body)["tuis"]
            .as_array()
            .expect("tuis")
            .iter()
            .map(|tui| tui["tui_id"].as_str().expect("an id").to_owned())
            .collect();
        assert_eq!(listed, [terminal_id], "{body}");
        assert!(
            core.ctx
                .tui_registry
                .list()
                .iter()
                .any(|tui| tui.client_kind.as_deref() == Some("gateway")),
            "the core registered the gateway's connection as a gateway's"
        );
        let (_, body) = get(&preview, "/api/sessions", Some(TOKEN)).await;
        assert_eq!(
            json_of(&body)["sessions"][0]["session_id"],
            "alpha",
            "{body}"
        );
        let (_, body) = get(&preview, "/api/events/history", Some(TOKEN)).await;
        assert_eq!(json_of(&body)["events"][0]["type"], "agent_start", "{body}");

        drop(terminal);
        core.stop().await;
    }

    // ── Per-session routes ───────────────────────────────────────

    /// The in-process gateway's answer to one per-session request.
    async fn in_process_session(
        state: &crate::AppState,
        method: &str,
        path: &str,
    ) -> (StatusCode, String) {
        use crate::api::{
            handle_api_session_delete, handle_api_session_messages, handle_api_session_state,
        };
        use axum::extract::Path as UrlPath;
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().unwrap(),
        );
        let rest = path.strip_prefix("/api/sessions/").expect("a session path");
        let state = State(state.clone());
        let response = match (method, rest.split_once('/')) {
            ("GET", Some((id, "messages"))) => {
                handle_api_session_messages(state, headers, UrlPath(id.to_owned()))
                    .await
                    .into_response()
            }
            ("GET", Some((id, "state"))) => {
                handle_api_session_state(state, headers, UrlPath(id.to_owned()))
                    .await
                    .into_response()
            }
            ("DELETE", None) => handle_api_session_delete(state, headers, UrlPath(rest.to_owned()))
                .await
                .into_response(),
            other => panic!("no in-process handler for {method} {path}: {other:?}"),
        };
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    /// The preview and the in-process gateway answer `method path` alike:
    /// the same status, the same body on success, the same error otherwise.
    async fn assert_same_answer(
        preview: &Router,
        state: &crate::AppState,
        method: &str,
        path: &str,
    ) -> serde_json::Value {
        let (status, served) = send(preview, method, path, Some(TOKEN)).await;
        let (in_process_status, in_process) = in_process_session(state, method, path).await;
        assert_eq!(
            status, in_process_status,
            "{method} {path}: the preview answered {served}, the in-process gateway {in_process}"
        );
        let (served, in_process) = (json_of(&served), json_of(&in_process));
        if status.is_success() {
            assert_eq!(served, in_process, "{method} {path}");
        } else {
            assert_eq!(served["error"], in_process["error"], "{method} {path}");
        }
        served
    }

    /// A core and the in-process gateway over one store, and a preview
    /// gateway attached to that core.
    async fn sessions_fixture(
        dir: &Path,
        adjust: impl FnOnce(&mut zeroclaw_config::schema::Config),
    ) -> (
        Core,
        Router,
        crate::AppState,
        Arc<dyn zeroclaw_infra::session_backend::SessionBackend>,
    ) {
        let mut ctx = Core::context_with(dir, adjust);
        let config = ctx.config.read().clone();
        let backend = zeroclaw_infra::make_session_backend(
            &config.data_dir,
            &config.channels.session_backend,
        )
        .expect("open the session store");
        Arc::get_mut(&mut ctx)
            .expect("a fresh context has one owner")
            .session_backend = Some(Arc::clone(&backend));
        let core = Core::serve(ctx).await;
        let preview = router(
            CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount),
            core.endpoint.clone(),
            None,
        );
        let state =
            crate::api::tests::test_state_with_session_backend(config, Arc::clone(&backend));
        (core, preview, state, backend)
    }

    fn append(
        backend: &Arc<dyn zeroclaw_infra::session_backend::SessionBackend>,
        key: &str,
        contents: &[&str],
    ) {
        for content in contents {
            backend
                .append(key, &zeroclaw_providers::ChatMessage::user(*content))
                .unwrap();
        }
        backend.set_session_agent_alias(key, "main").unwrap();
    }

    fn transcript(body: &serde_json::Value) -> Vec<String> {
        body["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("no messages: {body}"))
            .iter()
            .map(|row| row["content"].as_str().expect("content").to_owned())
            .collect()
    }

    /// Messages, state and delete answer through the preview exactly as the
    /// in-process gateway answers for the same store, and act on the row the
    /// in-process gateway selects even where the core's own id resolution
    /// would pick another.
    #[tokio::test]
    async fn the_session_routes_answer_as_the_in_process_gateway_does() {
        let tmp = tempfile::tempdir().unwrap();
        let (core, preview, state, backend) = sessions_fixture(tmp.path(), |_| {}).await;
        append(
            &backend,
            "gw_alpha",
            &["gateway question", "gateway answer"],
        );
        backend
            .set_session_state("gw_alpha", "running", Some("turn-7"))
            .unwrap();
        // The core resolves a plain id `alpha` as `rpc_alpha` first, and the
        // gateway key `gw_alpha` as `rpc_gw_alpha` first.
        append(&backend, "rpc_alpha", &["rpc conversation"]);
        append(
            &backend,
            "rpc_gw_alpha",
            &["rpc conversation under the gateway's key"],
        );
        // A dotted id lives under its sanitized key.
        append(&backend, "gw_team_alpha", &["dotted"]);

        for path in [
            "/api/sessions/alpha/messages",
            "/api/sessions/alpha/state",
            "/api/sessions/team.alpha/messages",
        ] {
            let (status, body) = get(&preview, path, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
        }
        let (status, _) = send(&preview, "DELETE", "/api/sessions/alpha", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        for id in [
            "alpha",
            "gw_alpha",
            "team.alpha",
            "gw_team_alpha",
            "missing",
        ] {
            assert_same_answer(
                &preview,
                &state,
                "GET",
                &format!("/api/sessions/{id}/messages"),
            )
            .await;
            assert_same_answer(
                &preview,
                &state,
                "GET",
                &format!("/api/sessions/{id}/state"),
            )
            .await;
        }
        let messages =
            assert_same_answer(&preview, &state, "GET", "/api/sessions/alpha/messages").await;
        assert_eq!(
            transcript(&messages),
            ["gateway question", "gateway answer"]
        );
        assert!(
            messages["messages"][0]["created_at"].is_string(),
            "{messages}"
        );
        let state_body =
            assert_same_answer(&preview, &state, "GET", "/api/sessions/gw_alpha/state").await;
        assert_eq!(state_body["state"], "running");
        assert_eq!(state_body["turn_id"], "turn-7");
        let dotted =
            assert_same_answer(&preview, &state, "GET", "/api/sessions/team.alpha/messages").await;
        assert_eq!(transcript(&dotted), ["dotted"]);
        let (status, _) = get(&preview, "/api/sessions/missing/state", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Delete through the preview, then the same state again for the
        // in-process gateway: the same answer and the same row removed.
        let (status, served) = send(&preview, "DELETE", "/api/sessions/alpha", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{served}");
        assert!(!backend.session_exists("gw_alpha"));
        append(&backend, "gw_alpha", &["gateway question"]);
        let (status, in_process) =
            in_process_session(&state, "DELETE", "/api/sessions/alpha").await;
        assert_eq!(status, StatusCode::OK, "{in_process}");
        assert_eq!(json_of(&served), json_of(&in_process));
        assert!(!backend.session_exists("gw_alpha"));
        for kept in ["rpc_alpha", "rpc_gw_alpha", "gw_team_alpha"] {
            assert!(backend.session_exists(kept), "{kept} was not the row named");
        }
        // Nothing left under that id: both say so.
        assert_same_answer(&preview, &state, "DELETE", "/api/sessions/alpha").await;
        let (status, _) = send(&preview, "DELETE", "/api/sessions/alpha", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, body) =
            send(&preview, "DELETE", "/api/sessions/team.alpha", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!backend.session_exists("gw_team_alpha"));
        assert!(backend.session_exists("rpc_alpha"));
        core.stop().await;
    }

    /// A bearer revoked after the preview pooled a connection for it is
    /// refused by the core at the operation itself: the delete removes
    /// nothing and the transcript is no longer served.
    #[tokio::test]
    async fn a_revoked_bearer_with_a_pooled_connection_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (core, preview, _, backend) = sessions_fixture(tmp.path(), |_| {}).await;
        append(&backend, "gw_alpha", &["kept"]);
        let (status, body) = get(&preview, "/api/sessions/alpha/messages", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        assert!(core.ctx.auth.pairing().revoke_token(TOKEN));
        let (status, body) = send(&preview, "DELETE", "/api/sessions/alpha", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(json_of(&body)["code"], "auth_required");
        assert!(
            backend.session_exists("gw_alpha"),
            "a refused caller deletes nothing"
        );
        for path in ["/api/sessions/alpha/messages", "/api/sessions/alpha/state"] {
            let (status, body) = get(&preview, path, Some(TOKEN)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {body}");
        }
        core.stop().await;
    }

    /// A transcript larger than one RPC frame is served whole, as the
    /// in-process gateway serves it; one message no frame can carry is named.
    #[tokio::test]
    async fn a_transcript_larger_than_an_rpc_frame_is_served_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let (core, preview, state, backend) = sessions_fixture(tmp.path(), |_| {}).await;
        let chunk = "x".repeat(60 * 1024);
        let contents: Vec<String> = (0..150).map(|i| format!("{i:03}{chunk}")).collect();
        for content in &contents {
            backend
                .append(
                    "gw_big",
                    &zeroclaw_providers::ChatMessage::user(content.as_str()),
                )
                .unwrap();
        }
        let body = assert_same_answer(&preview, &state, "GET", "/api/sessions/big/messages").await;
        assert_eq!(transcript(&body), contents);

        backend
            .append(
                "gw_huge",
                &zeroclaw_providers::ChatMessage::user("y".repeat(7 * 1024 * 1024 + 1)),
            )
            .unwrap();
        let (status, body) = get(&preview, "/api/sessions/huge/messages", Some(TOKEN)).await;
        assert_eq!(
            status,
            StatusCode::BAD_GATEWAY,
            "{}",
            &body[..body.len().min(300)]
        );
        assert_eq!(json_of(&body)["code"], "message_too_large");
        core.stop().await;
    }

    /// An identity provider that introspects `<user>-token` as `<user>`, in
    /// the group the fixture maps to session read and delete grants.
    async fn session_users_idp(users: &[&str]) -> wiremock::MockServer {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        // Owned, not pooled: the issuer's port stays this test's.
        let server = MockServer::builder().start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": issuer,
                "introspection_endpoint": format!("{issuer}/introspect"),
            })))
            .mount(&server)
            .await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for user in users {
            Mock::given(method("POST"))
                .and(path("/introspect"))
                .and(body_string_contains(format!("token={user}-token")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "active": true,
                    "token_type": "Bearer",
                    "client_id": "gw",
                    "iss": issuer,
                    "sub": user,
                    "aud": "zeroclaw",
                    "exp": now + 600,
                    "groups": [if *user == "no-read" { "no-sessions" } else { "sessions" }],
                })))
                .mount(&server)
                .await;
        }
        server
    }

    fn scoped_session_users(issuer: String) -> impl FnOnce(&mut zeroclaw_config::schema::Config) {
        use std::collections::HashMap;
        use zeroclaw_api::grants::{Resource, Verb};
        use zeroclaw_config::schema::{OidcConfig, OidcValidation, PermissionProfileConfig};
        move |config| {
            config.oidc.insert(
                "test".into(),
                OidcConfig {
                    issuer,
                    audience: "zeroclaw".into(),
                    client_id: "gw".into(),
                    client_secret: Some("s3cret".into()),
                    validation: OidcValidation::Introspection,
                    claim_path: "groups".into(),
                    profile_map: HashMap::from([(
                        "sessions".to_string(),
                        "session-user".to_string(),
                    )]),
                    interactive_clients: vec!["gw".into()],
                    ..OidcConfig::default()
                },
            );
            config.permission_profiles.insert(
                "session-user".into(),
                PermissionProfileConfig {
                    allowed_agents: vec!["*".into()],
                    // System:Read is what the core-link check asks for.
                    grants: HashMap::from([
                        (Resource::Sessions, vec![Verb::Read, Verb::Delete]),
                        (Resource::System, vec![Verb::Read]),
                    ]),
                    ..PermissionProfileConfig::default()
                },
            );
        }
    }

    /// `method path` as an OIDC user of the fixture's provider.
    async fn send_as(
        router: &Router,
        method: &str,
        path: &str,
        user: &str,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {user}-token"))
            .header(crate::principal_gate::AUTH_PROVIDER_HEADER, "oidc.test")
            .body(Body::empty())
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, json_of(&String::from_utf8_lossy(&body)))
    }

    /// Both daemon transports apply the same principal and agent selectors;
    /// a pool connection does not acquire access to another principal's rows.
    #[tokio::test]
    async fn credential_bound_session_visibility_matches_on_both_transports() {
        use zeroclaw_api::grants::{Resource, Verb};
        use zeroclaw_config::schema::PermissionProfileConfig;
        use zeroclaw_runtime::rpc::inproc::InprocConnector;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32)).unwrap();
        let idp = session_users_idp(&["alice", "bob", "no-read"]).await;
        let (core, local_preview, state, backend) = sessions_fixture(tmp.path(), |config| {
            scoped_session_users(idp.uri())(config);
            config.create_map_key("agents", "main").unwrap();
            config.create_map_key("agents", "other").unwrap();
            config.agents.get_mut("main").unwrap().channels = vec!["discord.ops".into()];
            config
                .permission_profiles
                .get_mut("session-user")
                .unwrap()
                .allowed_agents = vec!["main".into()];
            config.permission_profiles.insert(
                "no-sessions".into(),
                PermissionProfileConfig {
                    grants: std::collections::HashMap::from([(Resource::System, vec![Verb::Read])]),
                    ..Default::default()
                },
            );
            config
                .oidc
                .get_mut("test")
                .unwrap()
                .profile_map
                .insert("no-sessions".into(), "no-sessions".into());
        })
        .await;
        assert!(core.ctx.tui_registry.signing_is_enabled());
        let stop = CancellationToken::new();
        let connector = InprocConnector::new(stop.clone());
        connector.bind(Arc::clone(&core.ctx));
        let inproc = CoreRpc::inproc(connector, || true);
        let inproc_preview = router(inproc.clone(), core.endpoint.clone(), None);
        let mut principals = std::collections::HashMap::new();
        for user in ["alice", "bob"] {
            let (status, link) = send_as(&inproc_preview, "GET", CORE_LINK_PATH, user).await;
            assert_eq!(status, StatusCode::OK, "{link}");
            principals.insert(user, link["principal_id"].as_str().unwrap().to_owned());
        }
        for (key, owner, alias) in [
            ("gw_alice", Some("alice"), "main"),
            ("gw_bob", Some("bob"), "main"),
            ("gw_disallowed", Some("alice"), "other"),
            ("gw_operator", None, "main"),
        ] {
            append(&backend, key, &[key]);
            backend.set_session_agent_alias(key, alias).unwrap();
            backend.set_session_state(key, "idle", None).unwrap();
            if let Some(owner) = owner {
                backend
                    .set_session_principal(key, &principals[owner])
                    .unwrap();
            }
        }
        backend
            .append(
                "discord_owned",
                &zeroclaw_providers::ChatMessage::user("channel-owned"),
            )
            .unwrap();
        backend
            .set_session_context(
                "discord_owned",
                zeroclaw_infra::session_backend::SessionContext {
                    channel_id: Some("discord.ops"),
                    ..Default::default()
                },
            )
            .unwrap();
        backend
            .set_session_principal("discord_owned", &principals["alice"])
            .unwrap();
        let acp = core.ctx.acp_session_store.as_ref().unwrap();
        for (sid, owner, alias) in [
            ("acp-alice", Some("alice"), "main"),
            ("acp-bob", Some("bob"), "main"),
            ("acp-disallowed", Some("alice"), "other"),
            ("acp-operator", None, "main"),
        ] {
            acp.create_session(
                sid,
                alias,
                "/fixture-workspace",
                owner.map(|owner| principals[owner].as_str()),
            )
            .unwrap();
        }
        let local_rpc = CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount);
        for user in ["alice", "bob"] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {user}-token").parse().unwrap(),
            );
            headers.insert(
                crate::principal_gate::AUTH_PROVIDER_HEADER,
                "oidc.test".parse().unwrap(),
            );
            let CoreAccess::Core(local_call) = local_rpc.access(&headers).await.unwrap() else {
                unreachable!()
            };
            let CoreAccess::Core(duplex_call) = inproc.access(&headers).await.unwrap() else {
                unreachable!()
            };
            let local = local_call
                .request(Method::SessionListAcp, json!({}))
                .await
                .unwrap();
            let duplex = duplex_call
                .request(Method::SessionListAcp, json!({}))
                .await
                .unwrap();
            assert_eq!(duplex, local);
            assert_eq!(duplex["sessions"].as_array().unwrap().len(), 1, "{duplex}");
            assert_eq!(duplex["sessions"][0]["session_id"], format!("acp-{user}"));
            for method in [
                Method::SessionMessages,
                Method::SessionState,
                Method::SessionDelete,
            ] {
                let denied = duplex_call
                    .request(method, json!({"session_id":"acp-disallowed"}))
                    .await
                    .unwrap_err();
                assert!(matches!(denied, CoreError::Forbidden(_)), "{denied:?}");
            }
        }
        for user in ["alice", "bob"] {
            let (local_status, local) = send_as(&local_preview, "GET", "/api/sessions", user).await;
            let (inproc_status, duplex) =
                send_as(&inproc_preview, "GET", "/api/sessions", user).await;
            assert_eq!(local_status, StatusCode::OK, "{local}");
            assert_eq!(inproc_status, local_status);
            assert_eq!(duplex, local);
            let mut keys: Vec<_> = duplex["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_key"].as_str().unwrap())
                .collect();
            keys.sort_unstable();
            let expected = if user == "alice" {
                vec!["discord_owned", "gw_alice"]
            } else {
                vec!["gw_bob"]
            };
            assert_eq!(keys, expected);
        }
        let (status, local_operator) = get(&local_preview, "/api/sessions", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        let (status, duplex_operator) = get(&inproc_preview, "/api/sessions", Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json_of(&duplex_operator), json_of(&local_operator));
        let local_body = crate::api::handle_api_sessions_list(
            axum::extract::State(state.clone()),
            {
                let mut headers = axum::http::HeaderMap::new();
                headers.insert(
                    axum::http::header::AUTHORIZATION,
                    format!("Bearer {TOKEN}").parse().unwrap(),
                );
                headers
            },
            CoreAccess::InProcess,
        )
        .await
        .into_response()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
        assert_eq!(
            json_of(&local_operator),
            serde_json::from_slice::<serde_json::Value>(&local_body).unwrap()
        );
        for preview in [&local_preview, &inproc_preview] {
            for path in [
                "/api/sessions/disallowed/messages",
                "/api/sessions/disallowed/state",
            ] {
                let (denied_status, denied) = send_as(preview, "GET", path, "alice").await;
                let (missing_status, missing) =
                    send_as(preview, "GET", "/api/sessions/missing/messages", "alice").await;
                assert_eq!(denied_status, StatusCode::FORBIDDEN, "{denied}");
                assert_eq!(denied_status, missing_status);
                assert_eq!(denied["error"], missing["error"]);
            }
            let (status, body) =
                send_as(preview, "DELETE", "/api/sessions/disallowed", "alice").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert!(backend.session_exists("gw_disallowed"));
            let (status, body) = get(preview, "/api/sessions", None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
            let (status, body) = send_as(preview, "GET", "/api/sessions", "no-read").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        }
        assert!(core.ctx.auth.pairing().revoke_token(TOKEN));
        for preview in [&local_preview, &inproc_preview] {
            let (status, body) = get(preview, "/api/sessions", Some(TOKEN)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        }
        stop.cancel();
        core.stop().await;
    }

    /// A scoped principal can neither read, inspect nor delete another
    /// principal's session through the preview, cannot tell it from a
    /// session that does not exist, and is never handed someone else's row
    /// in place of its own.
    #[tokio::test]
    async fn another_principals_session_is_refused_and_left_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let idp = session_users_idp(&["alice", "bob"]).await;
        let (core, preview, _, backend) =
            sessions_fixture(tmp.path(), scoped_session_users(idp.uri())).await;
        let mut principal = std::collections::HashMap::new();
        for user in ["alice", "bob"] {
            let (status, link) = send_as(&preview, "GET", CORE_LINK_PATH, user).await;
            assert_eq!(status, StatusCode::OK, "{user}: {link}");
            let id = link["principal_id"]
                .as_str()
                .expect("a principal")
                .to_owned();
            assert_ne!(id, "shared-operator", "{user} is a scoped principal");
            principal.insert(user, id);
        }
        append(&backend, "gw_bob_notes", &["bob's secret"]);
        backend
            .set_session_principal("gw_bob_notes", &principal["bob"])
            .unwrap();
        append(&backend, "gw_alice_notes", &["alice's notes"]);
        backend
            .set_session_principal("gw_alice_notes", &principal["alice"])
            .unwrap();

        // The owner reads it: the grants are not what refuses alice below.
        let (status, body) =
            send_as(&preview, "GET", "/api/sessions/bob_notes/messages", "bob").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(transcript(&body), ["bob's secret"]);

        let (_, nothing) = send_as(&preview, "GET", "/api/sessions/nobody/messages", "alice").await;
        for (method, path) in [
            ("GET", "/api/sessions/bob_notes/messages"),
            ("GET", "/api/sessions/gw_bob_notes/messages"),
            ("GET", "/api/sessions/bob_notes/state"),
            ("DELETE", "/api/sessions/bob_notes"),
            ("DELETE", "/api/sessions/gw_bob_notes"),
        ] {
            let (status, body) = send_as(&preview, method, path, "alice").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
            assert_eq!(body["code"], "forbidden", "{method} {path}");
            assert_eq!(
                body["error"], nothing["error"],
                "{method} {path}: another's session looks like no session"
            );
            assert!(!body.to_string().contains("secret"), "{body}");
        }
        assert!(
            backend.session_exists("gw_bob_notes"),
            "bob's row is untouched"
        );
        assert_eq!(backend.load("gw_bob_notes").len(), 1);

        // Where both a raw key and a gateway key match the path, each
        // principal is served its own row, never the other's.
        append(&backend, "shared", &["bob's raw-key row"]);
        backend
            .set_session_principal("shared", &principal["bob"])
            .unwrap();
        append(&backend, "gw_shared", &["alice's gateway row"]);
        backend
            .set_session_principal("gw_shared", &principal["alice"])
            .unwrap();
        let (status, body) =
            send_as(&preview, "GET", "/api/sessions/shared/messages", "alice").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(transcript(&body), ["alice's gateway row"]);
        let (status, body) = send_as(&preview, "GET", "/api/sessions/shared/messages", "bob").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(transcript(&body), ["bob's raw-key row"]);
        let (status, body) = send_as(&preview, "DELETE", "/api/sessions/shared", "alice").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!backend.session_exists("gw_shared"));
        assert!(
            backend.session_exists("shared"),
            "bob's row survives alice's delete"
        );

        // Her own session she may delete.
        let (status, body) =
            send_as(&preview, "DELETE", "/api/sessions/alice_notes", "alice").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!backend.session_exists("gw_alice_notes"));
        core.stop().await;
    }

    #[tokio::test]
    async fn health_tells_the_dashboard_to_sign_in_and_names_no_path() {
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(tmp.path()).await;
        let router = router(
            CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount),
            core.endpoint.clone(),
            None,
        );
        let private = tmp.path().display().to_string();
        let expected_sign_in = serde_json::json!({
            "pairing_code": false, "bearer": true, "verify": CORE_LINK_PATH,
        });

        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let health = json_of(&body);
        assert_eq!(
            health["require_pairing"], true,
            "a dashboard must never read a missing field as pairing off"
        );
        assert_eq!(health["sign_in"], expected_sign_in);
        assert!(!body.contains(&private), "no path in public health: {body}");

        let headers = headers_of(&router, "/health").await;
        assert_eq!(headers["x-frame-options"], "DENY");
        assert!(headers.contains_key("content-security-policy"));

        let ctx = core.stop().await;
        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let health = json_of(&body);
        assert_eq!(health["require_pairing"], true);
        assert_eq!(health["sign_in"], expected_sign_in);
        assert_eq!(health["code"], "core_unavailable");
        assert!(health["hint"].is_string(), "{body}");
        assert!(!body.contains(&private), "no path in public health: {body}");
        drop(ctx);
    }

    /// The requests a fresh browser makes, in order: no stored token, then
    /// a token the core refuses, then one it accepts.
    #[tokio::test]
    async fn a_fresh_browser_signs_in_with_an_existing_token() {
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(tmp.path()).await;
        let router = router(
            CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount),
            core.endpoint.clone(),
            Some(web_dist(tmp.path())),
        );

        // The page loads; health says to sign in with a token, and where to
        // check it.
        let (status, _) = get(&router, "/", None).await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = get(&router, "/health", None).await;
        let health = json_of(&body);
        assert_eq!(health["require_pairing"], true);
        assert_eq!(health["sign_in"]["bearer"], true);
        assert_eq!(health["sign_in"]["pairing_code"], false);
        let verify = health["sign_in"]["verify"].as_str().unwrap().to_owned();

        // A token the core refuses: 401, so the sign-in screen stays.
        let (status, body) = get(&router, &verify, Some("zc_not_paired")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(json_of(&body)["code"], "auth_required");

        // The token the core accepts: data.
        let (status, body) = get(&router, &verify, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["principal_id"], "shared-operator");
        core.stop().await;
    }

    #[tokio::test]
    async fn the_health_probe_sends_nothing_and_its_connection_serves_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let recorder = Recorder::bind(tmp.path());
        let router = router(
            CoreRpc::local(recorder.endpoint.clone(), EndpointOwner::SameAccount),
            recorder.endpoint.clone(),
            None,
        );

        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(json_of(&body)["core"]["link"], "reachable");
        wait_for_connections(&recorder, 1).await;
        assert_eq!(
            recorder.connections()[0],
            Vec::<u8>::new(),
            "the probe writes nothing, not even a tokenless initialize"
        );

        // A domain call dials its own connection and presents its own
        // credential; the probe's connection is already gone.
        let (status, _) = get(&router, CORE_LINK_PATH, Some(TOKEN)).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "the recorder is no core"
        );
        wait_for_connections(&recorder, 2).await;
        let connections = recorder.connections();
        assert_eq!(connections.len(), 2);
        let frame: serde_json::Value = serde_json::from_slice(&connections[1]).unwrap();
        assert_eq!(frame["method"], "initialize");
        assert_eq!(frame["params"]["auth_token"], TOKEN);
    }

    #[tokio::test]
    async fn an_endpoint_that_fails_the_account_check_is_untrusted_and_hears_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let recorder = Recorder::bind(&shared);
        let router = router(
            CoreRpc::local(recorder.endpoint.clone(), EndpointOwner::SameAccount),
            recorder.endpoint.clone(),
            None,
        );

        let (status, body) = get(&router, "/health", None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        let health = json_of(&body);
        assert_eq!(health["code"], "core_untrusted_endpoint");
        assert_eq!(health["core"]["link"], "untrusted");
        assert!(!body.contains(&shared.display().to_string()), "{body}");

        let (status, body) = get(&router, CORE_LINK_PATH, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(json_of(&body)["code"], "core_untrusted_endpoint");
        assert!(json_of(&body)["hint"].is_string(), "{body}");

        wait_for_connections(&recorder, 2).await;
        assert!(
            recorder.connections().iter().all(Vec::is_empty),
            "nothing reaches an endpoint that fails the check"
        );
    }

    #[tokio::test]
    async fn no_dashboard_build_means_no_page_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(tmp.path()).await;
        let router = router(
            CoreRpc::local(core.endpoint.clone(), EndpointOwner::SameAccount),
            core.endpoint.clone(),
            None,
        );
        let (status, body) = get(&router, "/sessions", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(json_of(&body)["code"], "dashboard_unavailable");
        core.stop().await;
    }
}
