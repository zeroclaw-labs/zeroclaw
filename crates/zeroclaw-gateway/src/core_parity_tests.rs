//! Parity of the routes served through the core with their in-process bodies.
//!
//! Each test runs a route's handler twice over one shared state, once
//! in-process and once through the daemon's real in-process connector, and
//! requires the same status and body. The state the two paths read (cost
//! tracker, TUI registry, event history, pairing, SOP engine) is the same
//! instance, as it is under the daemon. The SOP runs socket is served on a
//! loopback port both ways and must send the same frames.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use zeroclaw_config::cost::{CostTracker, TokenUsage};
use zeroclaw_rpc_proto::types::CLIENT_KIND_GATEWAY;
use zeroclaw_runtime::rpc::context::RpcContext;
use zeroclaw_runtime::rpc::inproc::InprocConnector;
use zeroclaw_runtime::rpc::tui_identity::TuiEntry;

use crate::AppState;
use crate::api::{CostQuery, handle_api_cost, handle_api_health, handle_api_tuis};
use crate::core_rpc::{CoreAccess, CoreRpc};
use crate::sse::{EventBuffer, handle_events_history};
use crate::version::{CheckQuery, VersionCheckResponse, handle_version_check};
use zeroclaw_runtime::sop::{SopEngine, SopRunStatus, SopRunSummary};

const TOKEN: &str = "zc_parity_operator";

struct Harness {
    state: AppState,
    ctx: Arc<RpcContext>,
    core: CoreRpc,
    cancel: tokio_util::sync::CancellationToken,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Harness {
    fn new(cost_tracker: Option<Arc<CostTracker>>) -> Self {
        Self::with(cost_tracker, None)
    }

    /// A harness whose core and in-process state share `sop_engine`.
    fn with_sop_engine(sop_engine: Arc<std::sync::Mutex<SopEngine>>) -> Self {
        Self::with(None, Some(sop_engine))
    }

    fn with(
        cost_tracker: Option<Arc<CostTracker>>,
        sop_engine: Option<Arc<std::sync::Mutex<SopEngine>>>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = zeroclaw_config::schema::Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Default::default()
        };
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec![TOKEN.into()];
        // The daemon's TUI identity signing key. The in-process connector is
        // a non-local caller, and the core refuses its `initialize` while
        // signing is off.
        std::fs::write(dir.path().join(".secret_key"), "42".repeat(32)).expect("signing key");
        let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
            16,
            Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        ));
        let history = Arc::new(EventBuffer::new(16));
        let mut ctx = RpcContext::for_live_test(config.clone(), sessions);
        assert!(ctx.tui_registry.signing_is_enabled());
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context is unshared");
            ctx.cost_tracker = cost_tracker.clone();
            ctx.event_history = Some(Arc::clone(&history));
            ctx.sop_engine = sop_engine.clone();
        }
        let cancel = tokio_util::sync::CancellationToken::new();
        let connector = InprocConnector::new(cancel.clone());
        connector.bind(Arc::clone(&ctx));

        let mut state = crate::api::test_state(config);
        state.pairing = Arc::clone(ctx.auth.pairing());
        state.cost_tracker = cost_tracker;
        state.event_buffer = history;
        state.tui_registry = Some(Arc::clone(&ctx.tui_registry));
        state.sop_engine = sop_engine;

        Self {
            state,
            ctx,
            core: CoreRpc::inproc(connector, || true),
            cancel,
            _dir: dir,
        }
    }

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("header"),
        );
        headers
    }

    /// A request's access through the core, bound to the operator's bearer.
    async fn through_core(&self) -> CoreAccess {
        match self.core.access(&Self::headers()).await {
            Ok(access @ CoreAccess::Core(_)) => access,
            Ok(CoreAccess::InProcess) => panic!("served in-process"),
            Err(error) => panic!("no core access: {error:?}"),
        }
    }
}

async fn body_of(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).expect("JSON body");
    (status, body)
}

/// Both paths answer with the same status and body; returns the body.
fn assert_same(in_process: (StatusCode, Value), through_core: (StatusCode, Value)) -> Value {
    assert_eq!(in_process.0, StatusCode::OK, "{}", in_process.1);
    assert_eq!(
        through_core, in_process,
        "the core path must answer exactly as the in-process body"
    );
    in_process.1
}

#[tokio::test]
async fn api_health_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    zeroclaw_runtime::health::mark_component_ok("gw-parity-health");

    // The snapshot is process-wide and time-varying: its own `updated_at`
    // is the moment it was taken, and parallel tests may touch components.
    // Compare against an in-process body that did not move across the core
    // call, ignoring only that timestamp.
    let strip = |(status, mut body): (StatusCode, Value)| {
        if let Some(health) = body.get_mut("health").and_then(Value::as_object_mut) {
            health.remove("updated_at");
        }
        (status, body)
    };
    for attempt in 0.. {
        let before = strip(
            body_of(
                handle_api_health(
                    State(harness.state.clone()),
                    Harness::headers(),
                    CoreAccess::InProcess,
                )
                .await
                .into_response(),
            )
            .await,
        );
        let core = strip(
            body_of(
                handle_api_health(
                    State(harness.state.clone()),
                    Harness::headers(),
                    harness.through_core().await,
                )
                .await
                .into_response(),
            )
            .await,
        );
        let after = strip(
            body_of(
                handle_api_health(
                    State(harness.state.clone()),
                    Harness::headers(),
                    CoreAccess::InProcess,
                )
                .await
                .into_response(),
            )
            .await,
        );
        if before != after {
            assert!(attempt < 20, "the health snapshot never held still");
            continue;
        }
        let body = assert_same(before, core);
        assert!(body["health"].get("process").is_none());
        assert!(body["health"]["components"]["gw-parity-health"].is_object());
        break;
    }
}

#[tokio::test]
async fn api_tuis_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    harness.ctx.tui_registry.register(TuiEntry {
        tui_id: "tui_parity".into(),
        connected_at: chrono::Utc::now(),
        peer_label: "unix:parity".into(),
        transport: "unix".into(),
        env: HashMap::new(),
        client_kind: None,
    });

    // As in production, the gateway's own core connection exists (and the
    // core has registered it, as a gateway's) before either body is read.
    let through_core = harness.through_core().await;
    let own = harness
        .ctx
        .tui_registry
        .list()
        .into_iter()
        .find(|tui| tui.peer_label == zeroclaw_runtime::rpc::inproc::PEER_LABEL)
        .expect("the core registers the gateway's connection");
    assert_eq!(own.client_kind.as_deref(), Some(CLIENT_KIND_GATEWAY));
    let in_process = body_of(
        handle_api_tuis(
            State(harness.state.clone()),
            Harness::headers(),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_api_tuis(
            State(harness.state.clone()),
            Harness::headers(),
            through_core,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    let tuis = body["tuis"].as_array().expect("tuis");
    assert!(tuis.iter().any(|tui| tui["tui_id"] == "tui_parity"));
    assert!(
        tuis.iter().all(|tui| tui["tui_id"] != own.tui_id.as_str()),
        "the gateway's own connections are not terminals: {body}"
    );
    assert!(
        tuis.iter().all(|tui| tui.get("client_kind").is_none()),
        "the route's rows carry no client kind: {body}"
    );
}

#[tokio::test]
async fn events_history_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    harness.state.event_buffer.push(json!({
        "source": "observability",
        "type": "agent_start",
        "model": "parity-model",
    }));
    harness.state.event_buffer.push(json!({
        "type": "llm_request",
        "session_id": "withheld-from-the-global-view",
    }));

    let in_process = body_of(
        handle_events_history(
            State(harness.state.clone()),
            Harness::headers(),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_events_history(
            State(harness.state.clone()),
            Harness::headers(),
            harness.through_core().await,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["events"].as_array().expect("events").len(), 1);
}

fn usage_at(model: &str, cost: f64, when: chrono::DateTime<chrono::Utc>) -> TokenUsage {
    let mut usage = TokenUsage::new(model, 1_000, 500, 0, 0.0, 0.0, 0.0);
    usage.cost_usd = cost;
    usage.timestamp = when;
    usage
}

fn cost_tracker() -> (Arc<CostTracker>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = zeroclaw_config::schema::CostConfig {
        enabled: true,
        track_per_agent: true,
        ..Default::default()
    };
    let tracker = Arc::new(CostTracker::new(config, dir.path()).expect("tracker"));
    let now = chrono::Utc::now();
    tracker
        .record_usage_with_agent(usage_at("parity/today", 0.25, now), Some("alpha"))
        .expect("record");
    // A record from well before the current month: only an all-time
    // summary includes it.
    tracker
        .record_usage_with_agent(
            usage_at("parity/last-year", 1.5, now - chrono::Duration::days(400)),
            Some("beta"),
        )
        .expect("record");
    (tracker, dir)
}

#[tokio::test]
async fn api_cost_through_the_core_matches_the_in_process_body() {
    let (tracker, _dir) = cost_tracker();
    let harness = Harness::new(Some(tracker));
    let recent = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let cases: Vec<(&str, CostQuery)> = vec![
        ("no bounds", CostQuery::default()),
        (
            "agent",
            CostQuery {
                agent: Some("beta".into()),
                ..Default::default()
            },
        ),
        (
            "blank agent",
            CostQuery {
                agent: Some(String::new()),
                ..Default::default()
            },
        ),
        (
            "unknown agent",
            CostQuery {
                agent: Some("nobody".into()),
                ..Default::default()
            },
        ),
        (
            "agent ignores the window",
            CostQuery {
                agent: Some("alpha".into()),
                from: Some(recent.clone()),
                to: None,
            },
        ),
        (
            "from",
            CostQuery {
                from: Some(recent.clone()),
                ..Default::default()
            },
        ),
        (
            "to",
            CostQuery {
                to: Some(recent.clone()),
                ..Default::default()
            },
        ),
        (
            "unparsable from reads as absent",
            CostQuery {
                from: Some("yesterday".into()),
                ..Default::default()
            },
        ),
        (
            "unparsable from, valid to",
            CostQuery {
                from: Some("yesterday".into()),
                to: Some(recent.clone()),
                agent: None,
            },
        ),
    ];

    for (case, query) in cases {
        let in_process = body_of(
            handle_api_cost(
                State(harness.state.clone()),
                Harness::headers(),
                Query(query.clone()),
                CoreAccess::InProcess,
            )
            .await
            .into_response(),
        )
        .await;
        let core = body_of(
            handle_api_cost(
                State(harness.state.clone()),
                Harness::headers(),
                Query(query),
                harness.through_core().await,
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(core, in_process, "{case}");
        assert_eq!(in_process.0, StatusCode::OK, "{case}: {}", in_process.1);
        if case == "no bounds" {
            assert!(
                in_process.1["cost"]["by_model"]
                    .get("parity/last-year")
                    .is_some(),
                "the unbounded summary is all-time: {}",
                in_process.1
            );
        }
    }
}

#[tokio::test]
async fn api_cost_with_tracking_disabled_matches_the_in_process_body() {
    let harness = Harness::new(None);
    let in_process = body_of(
        handle_api_cost(
            State(harness.state.clone()),
            Harness::headers(),
            Query(CostQuery::default()),
            CoreAccess::InProcess,
        )
        .await
        .into_response(),
    )
    .await;
    let core = body_of(
        handle_api_cost(
            State(harness.state.clone()),
            Harness::headers(),
            Query(CostQuery::default()),
            harness.through_core().await,
        )
        .await
        .into_response(),
    )
    .await;
    let body = assert_same(in_process, core);
    assert_eq!(body["cost"]["request_count"], 0);
}

/// The latest release every test in this crate records as the core's cached
/// check, so tests that read the cache in parallel agree.
pub(crate) fn latest_release() -> VersionCheckResponse {
    VersionCheckResponse {
        current_version: env!("CARGO_PKG_VERSION").into(),
        latest_version: Some("99.0.0".into()),
        is_newer: true,
        release_url: Some("https://example.com/releases/99.0.0".into()),
        release_notes: Some("- parity".into()),
        published_at: Some("2026-10-01T00:00:00Z".into()),
        error: None,
    }
}

#[tokio::test]
async fn version_check_through_the_core_matches_the_in_process_body() {
    let harness = Harness::new(None);
    zeroclaw_runtime::update_check::remember_latest_check(latest_release());
    let cases = [
        ("the cached latest release", CheckQuery::default()),
        // A specific release is never cached, so both paths run the check.
        // Here it fails (the test binary refuses the arguments), and a
        // failed check is a 200 body with `error`, never an error status.
        (
            "a failed check",
            CheckQuery {
                force: false,
                version: Some("v0.0.0-unreleased".into()),
            },
        ),
    ];
    for (case, query) in cases {
        let in_process = body_of(
            handle_version_check(
                State(harness.state.clone()),
                Harness::headers(),
                Query(query.clone()),
                CoreAccess::InProcess,
            )
            .await
            .into_response(),
        )
        .await;
        let core = body_of(
            handle_version_check(
                State(harness.state.clone()),
                Harness::headers(),
                Query(query),
                harness.through_core().await,
            )
            .await
            .into_response(),
        )
        .await;
        let body = assert_same(in_process, core);
        if case == "a failed check" {
            assert!(body["error"].is_string(), "{case}: {body}");
            assert!(body["latest_version"].is_null(), "{case}: {body}");
        } else {
            assert_eq!(body, serde_json::to_value(latest_release()).unwrap());
        }
    }
}

// ── /ws/sops/runs ────────────────────────────────────────────────

pub(crate) type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Serve `router` on a loopback port.
pub(crate) async fn serve(router: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let address = listener.local_addr().expect("local address");
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, router).await;
    });
    address
}

/// Serve the in-process gateway's SOP runs socket over `core`: in-process
/// with [`CoreRpc::default`], through the core with a harness's handle.
pub(crate) async fn serve_sop_runs(state: AppState, core: CoreRpc) -> std::net::SocketAddr {
    serve(
        axum::Router::new()
            .route(
                "/ws/sops/runs",
                axum::routing::get(crate::ws_sop_runs::handle_ws_sop_runs),
            )
            .layer(axum::Extension(core))
            .with_state(state),
    )
    .await
}

/// Open the socket as the dashboard does: the bearer rides in the
/// subprotocol list, next to `zeroclaw.v1`.
pub(crate) async fn open_sop_runs(
    address: std::net::SocketAddr,
    bearer: Option<&str>,
) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut request = format!("ws://{address}/ws/sops/runs")
        .into_client_request()
        .expect("a valid request");
    let protocols = match bearer {
        Some(token) => format!("zeroclaw.v1, bearer.{token}"),
        None => "zeroclaw.v1".to_owned(),
    };
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&protocols).expect("header"),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

/// The next JSON frame, or `None` once the server closes the socket.
pub(crate) async fn next_frame(socket: &mut Socket) -> Option<Value> {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("a frame or a close within 5s");
        match message {
            Some(Ok(Message::Text(text))) => {
                return Some(serde_json::from_str(&text).expect("a JSON frame"));
            }
            Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
            Some(Ok(_)) => {}
        }
    }
}

/// The next frame from each socket, required to be the same.
pub(crate) async fn same_next_frame(in_process: &mut Socket, core: &mut Socket) -> Option<Value> {
    let expected = next_frame(in_process).await;
    let served = next_frame(core).await;
    assert_eq!(
        served, expected,
        "the socket through the core must send the in-process frame"
    );
    expected
}

pub(crate) fn run_change(run_id: &str, status: SopRunStatus) -> SopRunSummary {
    SopRunSummary {
        run_id: run_id.into(),
        sop_name: "parity-sop".into(),
        status,
        current_step: 1,
        total_steps: 2,
        started_at: "2026-10-01T00:00:00Z".into(),
        completed_at: None,
        trigger_source: "manual".into(),
        active: true,
    }
}

/// An engine publishing its run changes on the returned sender. One deep,
/// so three changes sent at once overflow it the same way on both paths.
pub(crate) fn sop_engine() -> (
    Arc<std::sync::Mutex<SopEngine>>,
    tokio::sync::broadcast::Sender<SopRunSummary>,
) {
    let (changes, _) = tokio::sync::broadcast::channel(1);
    let engine = SopEngine::new(zeroclaw_config::schema::SopConfig::default())
        .with_run_notifier(changes.clone());
    (Arc::new(std::sync::Mutex::new(engine)), changes)
}

#[tokio::test]
async fn sop_runs_socket_through_the_core_sends_the_in_process_frames() {
    let (engine, changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let in_process_at = serve_sop_runs(harness.state.clone(), CoreRpc::default()).await;
    let core_at = serve_sop_runs(harness.state.clone(), harness.core.clone()).await;

    let mut in_process = open_sop_runs(in_process_at, Some(TOKEN))
        .await
        .expect("the in-process socket opens");
    let mut core = open_sop_runs(core_at, Some(TOKEN))
        .await
        .expect("the socket through the core opens");
    let snapshot = same_next_frame(&mut in_process, &mut core).await;
    assert_eq!(snapshot, Some(json!({"type": "snapshot", "runs": []})));

    let running = run_change("parity-run", SopRunStatus::Running);
    changes.send(running.clone()).expect("both sockets listen");
    let changed = same_next_frame(&mut in_process, &mut core).await;
    assert_eq!(changed, Some(json!({"type": "run", "run": running})));

    // Three changes before either reader runs: the one-deep feed keeps the
    // last, and both sockets report the two they missed before it.
    for n in 1..=3 {
        changes
            .send(run_change(&format!("burst-{n}"), SopRunStatus::Completed))
            .expect("both sockets listen");
    }
    let lagged = same_next_frame(&mut in_process, &mut core).await;
    assert_eq!(lagged, Some(json!({"type": "lagged", "missed": 2})));
    let last = same_next_frame(&mut in_process, &mut core).await;
    assert_eq!(last.expect("a frame")["run"]["run_id"], "burst-3");

    // Without a bearer the core path refuses before the upgrade.
    assert!(open_sop_runs(core_at, None).await.is_err());

    // Closing both sockets lets go of both feeds: the in-process socket's
    // receiver, and the core's subscription the gateway cancels.
    in_process
        .close(None)
        .await
        .expect("close the in-process socket");
    core.close(None)
        .await
        .expect("close the socket through the core");
    feed_released(&changes).await;
}

/// Wait until nothing listens on the engine's run feed.
async fn feed_released(changes: &tokio::sync::broadcast::Sender<SopRunSummary>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while changes.receiver_count() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} run feed listener(s) left behind",
            changes.receiver_count()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A subscription whose socket never ran, because the upgrade did not
/// complete, is still cancelled on the core. Another request holds the same
/// connection meanwhile, so the feed ends because it was cancelled, not
/// because its connection closed, and the connection keeps serving.
#[tokio::test]
async fn an_abandoned_sop_runs_subscription_is_cancelled_on_the_core() {
    let (engine, changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let CoreAccess::Core(keeper) = harness.through_core().await else {
        panic!("served in-process");
    };
    let CoreAccess::Core(call) = harness.through_core().await else {
        panic!("served in-process");
    };
    let opened = crate::ws_sop_runs::subscribe(call)
        .await
        .expect("the core opens the feed");
    assert!(matches!(opened, crate::ws_sop_runs::Opened::Feed(_)));
    assert_eq!(changes.receiver_count(), 1, "the core's feed is armed");
    drop(opened);
    feed_released(&changes).await;
    keeper
        .request(zeroclaw_rpc_client::Method::Status, json!({}))
        .await
        .expect("the shared connection keeps serving");
}

/// A dialer that holds the core's first reply carrying a subscription id
/// until released: the core has opened the subscription, and the gateway's
/// subscribe request is still waiting for its answer.
struct HoldSubscribeReply {
    connector: InprocConnector,
    held: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    dials: Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::core_rpc::Dial for HoldSubscribeReply {
    fn dial(&self) -> crate::core_rpc::DialFuture<'_> {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        self.dials.fetch_add(1, Ordering::SeqCst);
        let (held, release) = (Arc::clone(&self.held), Arc::clone(&self.release));
        Box::pin(async move {
            let core = self.connector.connect().await?;
            let (gateway, proxy) = tokio::io::duplex(64 * 1024);
            let (mut from_gateway, mut to_gateway) = tokio::io::split(proxy);
            let (from_core, mut to_core) = tokio::io::split(core);
            zeroclaw_spawn::spawn!(async move {
                let _ = tokio::io::copy(&mut from_gateway, &mut to_core).await;
                let _ = to_core.shutdown().await;
            });
            zeroclaw_spawn::spawn!(async move {
                let mut lines = tokio::io::BufReader::new(from_core).lines();
                let mut holding = true;
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply: Value = serde_json::from_str(&line).unwrap_or_default();
                    if holding && reply["result"]["subscription_id"].is_string() {
                        holding = false;
                        held.notify_one();
                        release.notified().await;
                    }
                    if to_gateway
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Some(gateway)
        })
    }
}

/// The gateway stops waiting for `sops/subscribe-runs` after the core has
/// opened the feed and before its answer arrives, as an abandoned handshake
/// does. Once the answer lands the feed is cancelled, on a connection
/// another request still holds and that keeps serving.
#[tokio::test]
async fn a_sop_runs_subscribe_abandoned_before_its_answer_leaves_no_core_feed() {
    use std::sync::atomic::Ordering;
    let (engine, changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let held = harness.holding_subscribe_answers();
    let keeper = held.call().await;
    let call = held.call().await;
    let opening = zeroclaw_spawn::spawn!(crate::ws_sop_runs::subscribe(call));
    held.answered().await;
    assert_eq!(
        changes.receiver_count(),
        1,
        "the core opened the feed before answering"
    );
    opening.abort();
    assert!(opening.await.unwrap_err().is_cancelled());
    held.release.notify_one();

    feed_released(&changes).await;
    keeper
        .request(zeroclaw_rpc_client::Method::Status, json!({}))
        .await
        .expect("the shared connection keeps serving");
    assert_eq!(
        held.dials.load(Ordering::SeqCst),
        1,
        "one connection throughout"
    );
}

/// A core handle whose connections hold the core's first subscribe answer
/// until released, from [`Harness::holding_subscribe_answers`].
struct HeldAnswers {
    core: CoreRpc,
    held: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    dials: Arc<std::sync::atomic::AtomicUsize>,
}

impl HeldAnswers {
    /// A request's connection, bound to the operator's bearer.
    async fn call(&self) -> crate::core_rpc::CoreCall {
        match self.core.access(&Harness::headers()).await {
            Ok(CoreAccess::Core(call)) => call,
            _ => panic!("no core access"),
        }
    }

    /// Resolves once the core has answered a subscribe and the answer is
    /// being held.
    async fn answered(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.held.notified())
            .await
            .expect("the core answered");
    }
}

impl Harness {
    /// A core handle over this harness's core whose connections hold the
    /// first subscribe answer until released.
    fn holding_subscribe_answers(&self) -> HeldAnswers {
        let connector = InprocConnector::new(self.cancel.clone());
        connector.bind(Arc::clone(&self.ctx));
        let held = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let dials = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let core = CoreRpc::over_dialer(HoldSubscribeReply {
            connector,
            held: Arc::clone(&held),
            release: Arc::clone(&release),
            dials: Arc::clone(&dials),
        });
        HeldAnswers {
            core,
            held,
            release,
            dials,
        }
    }

    /// `zeroclaw-gw`'s router over `core`, with `request_timeout`.
    fn preview(&self, core: CoreRpc, request_timeout: std::time::Duration) -> axum::Router {
        crate::preview::router(
            core,
            self._dir.path().join("unused.sock"),
            None,
            tokio::sync::watch::channel(false).0,
            request_timeout,
        )
    }
}

/// `zeroclaw-gw`'s request timeout ends a handshake whose subscribe the core
/// has not answered in time. The caller gets `408`, and the feed the core
/// opened meanwhile is cancelled once its answer lands, on a connection
/// another request still holds.
#[tokio::test]
async fn the_preview_timeout_ends_a_stalled_sop_runs_handshake_and_its_feed() {
    let (engine, changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let held = harness.holding_subscribe_answers();
    let keeper = held.call().await;
    let preview_at =
        serve(harness.preview(held.core.clone(), std::time::Duration::from_millis(300))).await;

    match open_sop_runs(preview_at, Some(TOKEN)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        }
        other => panic!("a handshake past the timeout is refused: {other:?}"),
    }
    held.answered().await;
    assert_eq!(changes.receiver_count(), 1, "the core opened the feed");
    held.release.notify_one();

    feed_released(&changes).await;
    keeper
        .request(zeroclaw_rpc_client::Method::Status, json!({}))
        .await
        .expect("the shared connection keeps serving");
    assert_eq!(
        held.dials.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the socket's bearer and the keeper's share one connection"
    );
}

/// The request timeout bounds the handshake, not the socket: an upgraded
/// socket keeps delivering past it.
#[tokio::test]
async fn the_preview_timeout_does_not_cut_an_open_sop_runs_socket() {
    let (engine, changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let preview_at =
        serve(harness.preview(harness.core.clone(), std::time::Duration::from_millis(300))).await;
    let mut socket = open_sop_runs(preview_at, Some(TOKEN))
        .await
        .expect("the socket opens");
    assert_eq!(
        next_frame(&mut socket).await,
        Some(json!({"type": "snapshot", "runs": []}))
    );

    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    let running = run_change("after-the-timeout", SopRunStatus::Running);
    changes
        .send(running.clone())
        .expect("the feed is still armed");
    assert_eq!(
        next_frame(&mut socket).await,
        Some(json!({"type": "run", "run": running}))
    );
}

/// Both routes sit behind `zeroclaw-gw`'s body limit: a request declaring a
/// body over it, the socket's upgrade included, is answered `413` before the
/// handler runs.
#[tokio::test]
async fn the_new_routes_sit_behind_the_preview_body_limit() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;
    let (engine, changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let preview = harness.preview(
        harness.core.clone(),
        std::time::Duration::from_secs(crate::REQUEST_TIMEOUT_SECS),
    );
    let oversized = vec![b' '; crate::MAX_BODY_SIZE + 1];
    for (path, upgrade) in [("/api/version/check", false), ("/ws/sops/runs", true)] {
        let mut request = Request::get(path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_LENGTH, oversized.len());
        if upgrade {
            request = request
                .header(header::CONNECTION, "upgrade")
                .header(header::UPGRADE, "websocket")
                .header(header::SEC_WEBSOCKET_VERSION, "13")
                .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");
        }
        let request = request.body(Body::from(oversized.clone())).unwrap();
        let response = preview.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE, "{path}");
    }
    assert_eq!(changes.receiver_count(), 0, "no feed was opened");
}

/// The subprotocol bearer counts only when the request has no
/// `Authorization` header: a refused header is not rescued by it.
#[tokio::test]
async fn an_authorization_header_outranks_the_subprotocol_bearer() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let (engine, _changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);
    let core_at = serve_sop_runs(harness.state.clone(), harness.core.clone()).await;
    let mut request = format!("ws://{core_at}/ws/sops/runs")
        .into_client_request()
        .expect("a valid request");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!("zeroclaw.v1, bearer.{TOKEN}")).expect("header"),
    );
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer zc_not_paired"),
    );
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("the refused header must decide: {other:?}"),
    }
}

#[tokio::test]
async fn sop_runs_socket_with_sops_disabled_matches_the_in_process_frame() {
    let harness = Harness::new(None);
    let in_process_at = serve_sop_runs(harness.state.clone(), CoreRpc::default()).await;
    let core_at = serve_sop_runs(harness.state.clone(), harness.core.clone()).await;
    let mut in_process = open_sop_runs(in_process_at, Some(TOKEN))
        .await
        .expect("the in-process socket opens");
    let mut core = open_sop_runs(core_at, Some(TOKEN))
        .await
        .expect("the socket through the core opens");
    let disabled = same_next_frame(&mut in_process, &mut core).await;
    assert_eq!(disabled, Some(json!({"type": "disabled"})));
    assert_eq!(same_next_frame(&mut in_process, &mut core).await, None);
}

/// The engine failure the core reports as `-32603 engine lock poisoned`
/// reaches the dashboard as the in-process socket's `error` frame, and the
/// socket closes after it on both paths.
#[tokio::test]
async fn sop_runs_socket_with_a_failed_engine_matches_the_in_process_frame() {
    let (engine, _changes) = sop_engine();
    let poisoner = Arc::clone(&engine);
    let _ = std::thread::spawn(move || {
        let _held = poisoner.lock().expect("the first lock succeeds");
        panic!("poison the engine lock for this test");
    })
    .join();
    assert!(engine.is_poisoned());
    let harness = Harness::with_sop_engine(engine);
    let in_process_at = serve_sop_runs(harness.state.clone(), CoreRpc::default()).await;
    let core_at = serve_sop_runs(harness.state.clone(), harness.core.clone()).await;
    let mut in_process = open_sop_runs(in_process_at, Some(TOKEN))
        .await
        .expect("the in-process socket opens");
    let mut core = open_sop_runs(core_at, Some(TOKEN))
        .await
        .expect("the socket through the core opens");
    let failed = same_next_frame(&mut in_process, &mut core).await;
    assert_eq!(
        failed,
        Some(json!({"type": "error", "error": "engine lock poisoned"}))
    );
    assert_eq!(same_next_frame(&mut in_process, &mut core).await, None);
}

/// The refusals the core path can tell apart keep the in-process status:
/// no bearer is `401` on both routes, and a socket request that is not an
/// upgrade is refused the same way before any credential is looked at. The
/// bodies differ: each path answers with its own refusal shape.
#[tokio::test]
async fn refusals_keep_the_in_process_status_through_the_core() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    let (engine, _changes) = sop_engine();
    let harness = Harness::with_sop_engine(engine);

    // `GET /api/version/check` without a bearer.
    let in_process = handle_version_check(
        State(harness.state.clone()),
        HeaderMap::new(),
        Query(CheckQuery::default()),
        CoreAccess::InProcess,
    )
    .await
    .into_response()
    .status();
    let through_core = match harness.core.access(&HeaderMap::new()).await {
        Err(error) => error.into_response().status(),
        Ok(_) => panic!("no bearer must not reach the core"),
    };
    assert_eq!(in_process, StatusCode::UNAUTHORIZED);
    assert_eq!(through_core, in_process);

    // `GET /ws/sops/runs` without a bearer.
    let in_process_at = serve_sop_runs(harness.state.clone(), CoreRpc::default()).await;
    let core_at = serve_sop_runs(harness.state.clone(), harness.core.clone()).await;
    let status = |opened: Result<Socket, tokio_tungstenite::tungstenite::Error>| match opened {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status(),
        other => panic!("a socket without a bearer must be refused: {other:?}"),
    };
    let in_process = status(open_sop_runs(in_process_at, None).await);
    let through_core = status(open_sop_runs(core_at, None).await);
    assert_eq!(in_process, StatusCode::UNAUTHORIZED);
    assert_eq!(through_core, in_process);

    // A plain `GET /ws/sops/runs` with a valid bearer but no upgrade.
    let router = |core: CoreRpc| {
        axum::Router::new()
            .route(
                "/ws/sops/runs",
                axum::routing::get(crate::ws_sop_runs::handle_ws_sop_runs),
            )
            .layer(axum::Extension(core))
            .with_state(harness.state.clone())
    };
    let plain = || {
        Request::get("/ws/sops/runs")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .expect("request")
    };
    let in_process = router(CoreRpc::default())
        .oneshot(plain())
        .await
        .expect("a response")
        .status();
    let through_core = router(harness.core.clone())
        .oneshot(plain())
        .await
        .expect("a response")
        .status();
    assert!(in_process.is_client_error(), "{in_process}");
    assert_eq!(through_core, in_process);
}
