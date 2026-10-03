use super::*;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, header};
use axum::routing::get;
use http_body_util::BodyExt as _;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt as _;

/// A core stand-in that speaks just enough of the protocol. It records the
/// params of every `initialize` it receives, serves each connection as the
/// credential that opened it, and answers every call with the connection's
/// number, so a test can tell which connection carried a request.
#[derive(Default)]
struct FakeCore {
    accepted: Mutex<HashSet<String>>,
    handshakes: Mutex<Vec<Value>>,
    dials: AtomicUsize,
    open: Mutex<HashSet<usize>>,
    down: AtomicBool,
    drop_connections: tokio::sync::Notify,
}

impl FakeCore {
    fn accepting(tokens: &[&str]) -> Arc<Self> {
        let core = Self::default();
        lock(&core.accepted).extend(tokens.iter().map(|t| (*t).to_owned()));
        Arc::new(core)
    }

    fn accepts(&self, token: &str) -> bool {
        lock(&self.accepted).contains(token)
    }

    fn revoke(&self, token: &str) {
        lock(&self.accepted).remove(token);
    }

    fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }

    fn is_open(&self, connection: usize) -> bool {
        lock(&self.open).contains(&connection)
    }

    /// The invariant under test: every handshake the core ever saw carried a
    /// non-empty bearer and the provider that must verify it.
    fn assert_no_credential_less_handshake(&self) {
        for params in lock(&self.handshakes).iter() {
            let token = params["auth_token"].as_str().unwrap_or_default();
            let provider = params["auth_provider"].as_str().unwrap_or_default();
            assert!(
                !token.is_empty() && !provider.is_empty(),
                "a handshake reached the core without a credential: {params}"
            );
        }
    }

    fn handshake_tokens(&self) -> Vec<String> {
        lock(&self.handshakes)
            .iter()
            .map(|params| params["auth_token"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
}

struct FakeDialer(Arc<FakeCore>);

impl Dial for FakeDialer {
    fn dial(&self) -> DialFuture<'_> {
        let core = Arc::clone(&self.0);
        Box::pin(async move {
            if core.down.load(Ordering::SeqCst) {
                return None;
            }
            let connection = core.dials.fetch_add(1, Ordering::SeqCst) + 1;
            let (client, server) = tokio::io::duplex(64 * 1024);
            lock(&core.open).insert(connection);
            zeroclaw_spawn::spawn!(serve(core, server, connection));
            Some(client)
        })
    }
}

/// What the core answers a scoped principal that asks for a daemon-wide
/// stream: every principal the fake core binds is scoped.
const SCOPED_STREAM_DENIAL: &str =
    "Scoped principals cannot read the daemon-wide log and event streams";

fn reply(id: &Value, result: Value) -> String {
    format!(
        "{}\n",
        json!({ "jsonrpc": "2.0", "id": id, "result": result })
    )
}

fn refuse(id: &Value, code: i32, message: &str) -> String {
    format!(
        "{}\n",
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
    )
}

async fn serve(core: Arc<FakeCore>, stream: DuplexStream, connection: usize) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let mut bound: Option<String> = None;
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line,
            () = core.drop_connections.notified() => break,
        };
        let Ok(Some(line)) = line else { break };
        let frame: Value = serde_json::from_str(&line).expect("client frames are JSON");
        let id = frame["id"].clone();
        let answer = match (frame["method"].as_str(), &bound) {
            // Like the real core, a later `initialize` re-authenticates and
            // rebinds the connection, so a pool that forwarded one would be
            // caught rather than masked.
            (Some("initialize"), _) => {
                lock(&core.handshakes).push(frame["params"].clone());
                let token = frame["params"]["auth_token"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if core.accepts(&token) {
                    let answer = reply(
                        &id,
                        json!({
                            "protocol_version": 1,
                            "server_version": "fake",
                            "server_pid": 1,
                            "principal_id": format!("principal-of-{token}"),
                        }),
                    );
                    bound = Some(token);
                    answer
                } else {
                    refuse(&id, AUTH_REQUIRED, "credential rejected")
                }
            }
            (Some(method), Some(token)) => {
                if !core.accepts(token) {
                    refuse(&id, AUTH_REQUIRED, "credential revoked")
                } else if method == Method::ConfigGet.wire_name() {
                    refuse(&id, FORBIDDEN, "no config grant")
                } else if method == Method::Health.wire_name() {
                    // Never answered: the request times out.
                    continue;
                } else if method == Method::EventsHistory.wire_name() {
                    refuse(&id, FORBIDDEN, SCOPED_STREAM_DENIAL)
                } else {
                    reply(&id, json!({ "connection": connection, "principal": token }))
                }
            }
            _ => refuse(&id, AUTH_REQUIRED, "initialize first"),
        };
        if write.write_all(answer.as_bytes()).await.is_err() {
            break;
        }
    }
    lock(&core.open).remove(&connection);
}

fn core_over(fake: &Arc<FakeCore>, pairing_required: bool, limits: PoolLimits) -> CoreRpc {
    CoreRpc::with_dialer(
        FakeDialer(Arc::clone(fake)),
        move || pairing_required,
        limits,
    )
}

fn pool_of(core: &CoreRpc) -> Arc<Pool> {
    Arc::clone(&core.seam.as_ref().expect("attached").pool)
}

fn headers(token: Option<&str>, provider: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("header"),
        );
    }
    if let Some(provider) = provider {
        headers.insert(
            AUTH_PROVIDER_HEADER,
            HeaderValue::from_str(provider).expect("header"),
        );
    }
    headers
}

async fn call_for(core: &CoreRpc, token: &str) -> CoreCall {
    match core.access(&headers(Some(token), None)).await {
        Ok(CoreAccess::Core(call)) => call,
        Ok(CoreAccess::InProcess) => panic!("{token}: served in-process"),
        Err(error) => panic!("{token}: {error:?}"),
    }
}

async fn connection_of(call: &CoreCall) -> u64 {
    call.request(Method::Status, json!({}))
        .await
        .expect("status")["connection"]
        .as_u64()
        .expect("connection number")
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let reached = tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(reached.is_ok(), "never observed: {what}");
}

fn status_of(error: &CoreError) -> (StatusCode, &'static str) {
    error.status()
}

// ── Credential binding ───────────────────────────────────────────

#[tokio::test]
async fn each_credential_gets_its_own_connection_and_never_uses_another() {
    let fake = FakeCore::accepting(&["zc_alice", "zc_bob"]);
    let core = core_over(&fake, true, PoolLimits::default());

    let alice = call_for(&core, "zc_alice").await;
    let bob = call_for(&core, "zc_bob").await;
    assert_eq!(alice.principal_id(), Some("principal-of-zc_alice"));
    assert_eq!(bob.principal_id(), Some("principal-of-zc_bob"));
    let alice_connection = connection_of(&alice).await;
    let bob_connection = connection_of(&bob).await;
    assert_ne!(alice_connection, bob_connection);

    // A later request with the same credential reuses its connection.
    let alice_again = call_for(&core, "zc_alice").await;
    assert_eq!(connection_of(&alice_again).await, alice_connection);
    assert_eq!(fake.dials(), 2);
    assert_eq!(fake.handshake_tokens(), ["zc_alice", "zc_bob"]);
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn one_credential_under_two_providers_is_two_connections() {
    let fake = FakeCore::accepting(&["zc_shared"]);
    let core = core_over(&fake, true, PoolLimits::default());

    let native = match core
        .access(&headers(Some("zc_shared"), Some("native")))
        .await
    {
        Ok(CoreAccess::Core(call)) => call,
        _ => panic!("native selection is served by the core"),
    };
    let oidc = match core
        .access(&headers(Some("zc_shared"), Some("oidc.corp")))
        .await
    {
        Ok(CoreAccess::Core(call)) => call,
        _ => panic!("oidc selection is served by the core"),
    };
    assert_ne!(connection_of(&native).await, connection_of(&oidc).await);
    let providers: Vec<String> = lock(&fake.handshakes)
        .iter()
        .map(|p| p["auth_provider"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(providers, ["native", "oidc.corp"]);
}

#[tokio::test]
async fn every_connection_declares_itself_a_gateway_client() {
    let fake = FakeCore::accepting(&["zc_alice", "zc_bob"]);
    let core = core_over(&fake, true, PoolLimits::default());

    call_for(&core, "zc_alice").await;
    match core
        .access(&headers(Some("zc_bob"), Some("oidc.corp")))
        .await
    {
        Ok(CoreAccess::Core(_)) => {}
        _ => panic!("oidc selection is served by the core"),
    }
    let kinds: Vec<Value> = lock(&fake.handshakes)
        .iter()
        .map(|params| params["clientCapabilities"]["client_kind"].clone())
        .collect();
    assert_eq!(kinds, [json!("gateway"), json!("gateway")]);
}

#[tokio::test]
async fn concurrent_first_requests_with_one_credential_share_one_dial() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());

    let (first, second) = tokio::join!(call_for(&core, "zc_alice"), call_for(&core, "zc_alice"));
    assert_eq!(connection_of(&first).await, connection_of(&second).await);
    assert_eq!(fake.dials(), 1);
}

// ── The no-tokenless invariant ───────────────────────────────────

#[tokio::test]
async fn an_unusable_credential_is_refused_before_any_connection() {
    let long = "a".repeat(MAX_BEARER_BYTES + 1);
    let cases: Vec<(&str, HeaderMap)> = vec![
        ("no Authorization header", HeaderMap::new()),
        ("empty bearer", headers(Some(""), None)),
        ("blank bearer", headers(Some("   "), None)),
        (
            "bearer with inner whitespace",
            headers(Some("two words"), None),
        ),
        ("over-long bearer", headers(Some(&long), None)),
        ("blank provider", headers(Some("zc_alice"), Some(""))),
        (
            "whitespace provider",
            headers(Some("zc_alice"), Some("   ")),
        ),
        (
            "transport provider",
            headers(Some("zc_alice"), Some("peercred")),
        ),
        (
            "unknown provider",
            headers(Some("zc_alice"), Some("service")),
        ),
        (
            "case-changed provider",
            headers(Some("zc_alice"), Some("NATIVE")),
        ),
        (
            "oidc with no alias",
            headers(Some("zc_alice"), Some("oidc.")),
        ),
        ("provider without a bearer", headers(None, Some("native"))),
    ];
    let mut basic = HeaderMap::new();
    basic.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Basic emM6"),
    );
    let mut undecodable = headers(Some("zc_alice"), None);
    undecodable.insert(
        AUTH_PROVIDER_HEADER,
        HeaderValue::from_bytes(b"oidc.\xff").expect("obs-text header"),
    );

    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());
    for (case, headers) in cases.iter().map(|(case, h)| (*case, h)).chain([
        ("basic scheme", &basic),
        ("undecodable provider", &undecodable),
    ]) {
        match core.access(headers).await {
            Err(error) => assert_eq!(
                status_of(&error),
                (StatusCode::UNAUTHORIZED, "auth_required"),
                "{case}"
            ),
            Ok(_) => panic!("{case}: must be refused"),
        }
    }
    assert_eq!(fake.dials(), 0, "no connection was opened for any of them");
}

#[tokio::test]
async fn with_pairing_disabled_only_an_unselected_request_stays_in_process() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, false, PoolLimits::default());

    for headers in [
        HeaderMap::new(),
        headers(Some("zc_alice"), None),
        headers(Some("not a token"), None),
    ] {
        assert!(matches!(
            core.access(&headers).await,
            Ok(CoreAccess::InProcess)
        ));
    }
    // A provider selection is a request to authenticate: it is never waved
    // through, and a bad one is refused.
    assert!(matches!(
        core.access(&headers(Some("zc_alice"), Some("peercred")))
            .await,
        Err(CoreError::AuthRequired(_))
    ));
    assert!(matches!(
        core.access(&headers(None, Some("native"))).await,
        Err(CoreError::AuthRequired(_))
    ));
    assert_eq!(fake.dials(), 0);

    let selected = core
        .access(&headers(Some("zc_alice"), Some("native")))
        .await;
    assert!(matches!(selected, Ok(CoreAccess::Core(_))));
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn a_credential_the_core_refuses_is_401_and_never_pooled() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());

    for _ in 0..2 {
        match core.access(&headers(Some("zc_forged"), None)).await {
            Err(error) => {
                assert_eq!(
                    status_of(&error),
                    (StatusCode::UNAUTHORIZED, "auth_required")
                )
            }
            Ok(_) => panic!("a refused credential must not be served"),
        }
    }
    assert_eq!(pool_of(&core).pooled(), 0);
    // Each attempt presented the same credential; none fell back to
    // presenting nothing.
    assert_eq!(fake.handshake_tokens(), ["zc_forged", "zc_forged"]);
    fake.assert_no_credential_less_handshake();
    wait_until("the refused connections close", || {
        lock(&fake.open).is_empty()
    })
    .await;
}

// ── Revocation, grants and failure ───────────────────────────────

#[tokio::test]
async fn a_revoked_credential_ends_its_connection_at_next_use() {
    let fake = FakeCore::accepting(&["zc_alice", "zc_bob"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let alice = call_for(&core, "zc_alice").await;
    let bob = call_for(&core, "zc_bob").await;
    let alice_connection = connection_of(&alice).await as usize;
    let bob_connection = connection_of(&bob).await;

    fake.revoke("zc_alice");
    let refused = alice
        .request(Method::Status, json!({}))
        .await
        .expect_err("the revoked credential is refused");
    assert_eq!(
        status_of(&refused),
        (StatusCode::UNAUTHORIZED, "auth_required")
    );
    wait_until("the revoked connection closes", || {
        !fake.is_open(alice_connection)
    })
    .await;
    assert_eq!(pool_of(&core).pooled(), 1, "only bob's connection remains");
    assert_eq!(connection_of(&bob).await, bob_connection);

    // The next request with the revoked credential dials again with it and
    // is refused at the handshake.
    assert!(matches!(
        core.access(&headers(Some("zc_alice"), None)).await,
        Err(CoreError::AuthRequired(_))
    ));
    assert_eq!(fake.handshake_tokens(), ["zc_alice", "zc_bob", "zc_alice"]);
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn a_missing_grant_is_403_and_the_next_request_dials_again() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let alice = call_for(&core, "zc_alice").await;

    let denied = alice
        .request(Method::ConfigGet, json!({}))
        .await
        .expect_err("the fake core denies config/get");
    assert_eq!(status_of(&denied), (StatusCode::FORBIDDEN, "forbidden"));
    assert_eq!(pool_of(&core).pooled(), 0);

    let again = call_for(&core, "zc_alice").await;
    connection_of(&again).await;
    assert_eq!(fake.handshake_tokens(), ["zc_alice", "zc_alice"]);
}

#[tokio::test]
async fn an_unreachable_core_is_503_never_401() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    fake.down.store(true, Ordering::SeqCst);
    let core = core_over(&fake, true, PoolLimits::default());

    let error = match core.access(&headers(Some("zc_alice"), None)).await {
        Err(error) => error,
        Ok(_) => panic!("no core to serve the request"),
    };
    assert_eq!(
        status_of(&error),
        (StatusCode::SERVICE_UNAVAILABLE, "core_unavailable")
    );
    // A bad credential is still a 401 while the core is down: the shape
    // checks run first, and nothing is dialed for them.
    let error = match core.access(&HeaderMap::new()).await {
        Err(error) => error,
        Ok(_) => panic!("refused"),
    };
    assert_eq!(
        status_of(&error),
        (StatusCode::UNAUTHORIZED, "auth_required")
    );
    assert_eq!(fake.dials(), 0);
}

#[tokio::test]
async fn a_lost_connection_is_503_and_the_next_request_redials_with_its_credential() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let alice = call_for(&core, "zc_alice").await;
    connection_of(&alice).await;

    fake.drop_connections.notify_waiters();
    wait_until("the core drops the connection", || {
        lock(&fake.open).is_empty()
    })
    .await;
    let lost = alice
        .request(Method::Status, json!({}))
        .await
        .expect_err("the connection is gone");
    assert_eq!(
        status_of(&lost),
        (StatusCode::SERVICE_UNAVAILABLE, "core_unavailable")
    );

    let again = call_for(&core, "zc_alice").await;
    connection_of(&again).await;
    assert_eq!(fake.handshake_tokens(), ["zc_alice", "zc_alice"]);
    fake.assert_no_credential_less_handshake();
}

#[tokio::test(start_paused = true)]
async fn the_pool_never_dials_on_its_own() {
    // No probe, heartbeat or reconnect loop exists that could open a
    // connection without a caller's credential: the only dial is a
    // request's, carrying that request's bearer.
    let fake = FakeCore::accepting(&["zc_alice"]);
    let limits = PoolLimits {
        sweep_interval: Duration::from_secs(1),
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    tokio::time::advance(Duration::from_secs(3600)).await;
    tokio::task::yield_now().await;
    assert_eq!(fake.dials(), 0, "an idle pool opened a connection");

    // A connection the core drops is not dialed again until a request
    // presents its credential.
    let alice = call_for(&core, "zc_alice").await;
    connection_of(&alice).await;
    drop(alice);
    fake.drop_connections.notify_waiters();
    wait_until("the core drops the connection", || {
        lock(&fake.open).is_empty()
    })
    .await;
    tokio::time::advance(Duration::from_secs(3600)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        fake.dials(),
        1,
        "a dropped connection was dialed again unasked"
    );
    assert_eq!(pool_of(&core).pooled(), 0);
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn a_scoped_principal_is_refused_the_global_event_history() {
    let fake = FakeCore::accepting(&["zc_scoped"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let access = match core.access(&headers(Some("zc_scoped"), None)).await {
        Ok(access @ CoreAccess::Core(_)) => access,
        _ => panic!("a scoped bearer is served by the core"),
    };
    let state = crate::api::test_state(zeroclaw_config::schema::Config::default());
    state
        .event_buffer
        .push(json!({ "source": "observability", "type": "agent_start" }));

    let response = crate::sse::handle_events_history(
        axum::extract::State(state),
        headers(Some("zc_scoped"), None),
        access,
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("JSON");
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["error"], SCOPED_STREAM_DENIAL);
    // The gateway's own buffer is never served in place of the refusal.
    assert!(body.get("events").is_none(), "{body}");
    assert_eq!(fake.handshake_tokens(), ["zc_scoped"]);
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn a_gateway_without_a_core_serves_in_process() {
    let core = CoreRpc::default();
    assert!(matches!(
        core.access(&HeaderMap::new()).await,
        Ok(CoreAccess::InProcess)
    ));
}

// ── Bounds ───────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn idle_connections_leave_the_pool() {
    let fake = FakeCore::accepting(&["zc_alice", "zc_bob"]);
    let limits = PoolLimits {
        max_credentials: 8,
        idle_timeout: Duration::from_secs(600),
        sweep_interval: Duration::from_secs(60),
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    let pool = pool_of(&core);

    let alice_connection = {
        let alice = call_for(&core, "zc_alice").await;
        connection_of(&alice).await as usize
    };
    tokio::time::advance(Duration::from_secs(360)).await;
    {
        let bob = call_for(&core, "zc_bob").await;
        connection_of(&bob).await;
    }
    tokio::time::advance(Duration::from_secs(300)).await;
    pool.sweep_idle();

    assert_eq!(pool.pooled(), 1, "alice was idle for 11 minutes, bob for 5");
    wait_until("alice's idle connection closes", || {
        !fake.is_open(alice_connection)
    })
    .await;

    // The background sweep does the same without being called.
    tokio::time::advance(Duration::from_secs(700)).await;
    wait_until("the sweep drops bob's connection", || pool.pooled() == 0).await;
}

#[tokio::test(start_paused = true)]
async fn the_cap_drops_the_least_recently_used_credential() {
    let fake = FakeCore::accepting(&["zc_alice", "zc_bob", "zc_carol"]);
    let limits = PoolLimits {
        max_credentials: 2,
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);

    let mut connections = HashMap::new();
    for token in ["zc_alice", "zc_bob", "zc_alice", "zc_carol"] {
        let call = call_for(&core, token).await;
        connections.insert(token, connection_of(&call).await as usize);
        tokio::time::advance(Duration::from_secs(1)).await;
    }

    assert_eq!(pool_of(&core).pooled(), 2);
    wait_until("bob's connection, used least recently, closes", || {
        !fake.is_open(connections["zc_bob"])
    })
    .await;
    assert!(fake.is_open(connections["zc_alice"]));
    assert!(fake.is_open(connections["zc_carol"]));
}

// ── The extractor ────────────────────────────────────────────────

async fn probe(access: CoreAccess) -> Response {
    match access {
        CoreAccess::InProcess => "in-process".into_response(),
        CoreAccess::Core(call) => match call.request(Method::Status, json!({})).await {
            Ok(result) => Json(result).into_response(),
            Err(error) => error.into_response(),
        },
    }
}

async fn get_probe(router: Router, headers: HeaderMap) -> (StatusCode, Value) {
    let mut request = Request::builder().uri("/probe");
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = router
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("infallible");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

#[tokio::test]
async fn the_extractor_answers_401_and_503_distinctly() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let router = Router::new()
        .route("/probe", get(probe))
        .layer(axum::Extension(core));

    let (status, body) = get_probe(router.clone(), HeaderMap::new()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "auth_required");
    assert_eq!(fake.dials(), 0);

    let (status, body) = get_probe(router.clone(), headers(Some("zc_alice"), None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["connection"], 1);

    fake.down.store(true, Ordering::SeqCst);
    let (status, body) = get_probe(router.clone(), headers(Some("zc_newcomer"), None)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "core_unavailable");

    // A route the core handle was never installed on fails closed.
    let unwired = Router::new().route("/probe", get(probe));
    let (status, body) = get_probe(unwired, headers(Some("zc_alice"), None)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "core_unavailable");
    fake.assert_no_credential_less_handshake();
}

#[test]
fn core_error_codes_map_to_their_http_statuses() {
    use zeroclaw_api::jsonrpc::error_codes::INTERNAL_ERROR;
    let cases = [
        (AUTH_REQUIRED, StatusCode::UNAUTHORIZED),
        (FORBIDDEN, StatusCode::FORBIDDEN),
        (SESSION_NOT_OWNED, StatusCode::FORBIDDEN),
        (INVALID_PARAMS, StatusCode::BAD_REQUEST),
        (SESSION_NOT_FOUND, StatusCode::NOT_FOUND),
        (SESSION_LIMIT_REACHED, StatusCode::TOO_MANY_REQUESTS),
        (SESSION_BUSY, StatusCode::CONFLICT),
        (METHOD_NOT_FOUND, StatusCode::SERVICE_UNAVAILABLE),
        (VERSION_MISMATCH, StatusCode::SERVICE_UNAVAILABLE),
        (CONNECTION_LIMIT_REACHED, StatusCode::SERVICE_UNAVAILABLE),
        (INTERNAL_ERROR, StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (code, status) in cases {
        let error = CoreError::from_rpc(JsonRpcError {
            code,
            message: "m".into(),
            data: None,
        });
        assert_eq!(error.status().0, status, "code {code}");
    }
}

// ── Against the daemon's real in-process connector ───────────────

struct CountingDial {
    connector: InprocConnector,
    dials: Arc<AtomicUsize>,
}

impl Dial for CountingDial {
    fn dial(&self) -> DialFuture<'_> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        self.connector.dial()
    }
}

/// Give the daemon directory behind `config` a TUI identity signing key, as
/// an installed daemon has. The in-process duplex is a non-local caller, and
/// the core refuses its `initialize` while signing is off, so a test of the
/// credential layer needs signing on to reach it.
fn with_daemon_signing_key(config: &zeroclaw_config::schema::Config) {
    let dir = config.config_path.parent().expect("config dir");
    std::fs::write(dir.join(".secret_key"), "42".repeat(32)).expect("signing key");
}

#[tokio::test]
async fn the_real_core_binds_each_bearer_and_revocation_ends_its_connection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config.gateway.require_pairing = true;
    config.gateway.paired_tokens = vec!["zc_gw_alice".into(), "zc_gw_bob".into()];
    with_daemon_signing_key(&config);
    let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
        16,
        Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        )),
    ));
    let ctx = zeroclaw_runtime::rpc::context::RpcContext::for_live_test(config, sessions);
    assert!(ctx.tui_registry.signing_is_enabled());
    let cancel = tokio_util::sync::CancellationToken::new();
    let connector = InprocConnector::new(cancel.clone());
    connector.bind(Arc::clone(&ctx));
    let dials = Arc::new(AtomicUsize::new(0));
    let core = CoreRpc::with_dialer(
        CountingDial {
            connector,
            dials: Arc::clone(&dials),
        },
        || true,
        PoolLimits::default(),
    );

    // Refused before the core: nothing dialed.
    assert!(matches!(
        core.access(&HeaderMap::new()).await,
        Err(CoreError::AuthRequired(_))
    ));
    assert_eq!(dials.load(Ordering::SeqCst), 0);

    // A forged bearer is refused by the core and never pooled.
    assert!(matches!(
        core.access(&headers(Some("zc_gw_forged"), None)).await,
        Err(CoreError::AuthRequired(_))
    ));
    assert_eq!(pool_of(&core).pooled(), 0);

    let alice = call_for(&core, "zc_gw_alice").await;
    let bob = call_for(&core, "zc_gw_bob").await;
    assert_eq!(alice.principal_id(), Some("shared-operator"));
    alice
        .request(Method::Status, json!({}))
        .await
        .expect("alice's status");
    bob.request(Method::Status, json!({}))
        .await
        .expect("bob's status");
    assert_eq!(pool_of(&core).pooled(), 2);

    assert!(ctx.auth.pairing().revoke_token("zc_gw_alice"));
    let refused = alice
        .request(Method::Status, json!({}))
        .await
        .expect_err("the revoked bearer is refused on its next use");
    assert_eq!(
        status_of(&refused),
        (StatusCode::UNAUTHORIZED, "auth_required")
    );
    assert_eq!(pool_of(&core).pooled(), 1);
    bob.request(Method::Status, json!({}))
        .await
        .expect("bob is unaffected");

    cancel.cancel();
}

// ── Review regressions ───────────────────────────────────────────

/// A real in-process core with two paired native bearers, counting dials.
fn real_pool() -> (
    tempfile::TempDir,
    Arc<zeroclaw_runtime::rpc::context::RpcContext>,
    tokio_util::sync::CancellationToken,
    CoreRpc,
    Arc<AtomicUsize>,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config.gateway.require_pairing = true;
    config.gateway.paired_tokens = vec!["zc_real_a".into(), "zc_real_b".into()];
    with_daemon_signing_key(&config);
    let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
        16,
        Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        )),
    ));
    let ctx = zeroclaw_runtime::rpc::context::RpcContext::for_live_test(config, sessions);
    assert!(ctx.tui_registry.signing_is_enabled());
    let cancel = tokio_util::sync::CancellationToken::new();
    let connector = InprocConnector::new(cancel.clone());
    connector.bind(Arc::clone(&ctx));
    let dials = Arc::new(AtomicUsize::new(0));
    let core = CoreRpc::with_dialer(
        CountingDial {
            connector,
            dials: Arc::clone(&dials),
        },
        || true,
        PoolLimits::default(),
    );
    (tmp, ctx, cancel, core, dials)
}

#[tokio::test]
async fn a_request_can_never_send_initialize() {
    let fake = FakeCore::accepting(&["zc_alice", "zc_bob"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let alice = call_for(&core, "zc_alice").await;

    let refused = alice
        .request(
            Method::Initialize,
            json!({ "protocol_version": 1, "auth_token": "zc_bob", "auth_provider": "native" }),
        )
        .await
        .expect_err("initialize belongs to the pool");
    assert!(matches!(&refused, CoreError::Rpc(e) if e.code == INVALID_REQUEST));
    let tokenless = alice
        .request(Method::Initialize, json!({ "protocol_version": 1 }))
        .await
        .expect_err("a tokenless initialize is refused the same way");
    assert!(matches!(&tokenless, CoreError::Rpc(e) if e.code == INVALID_REQUEST));

    // Neither reached the core, and the connection is still alice's.
    assert_eq!(fake.handshake_tokens(), ["zc_alice"]);
    let status = alice
        .request(Method::Status, json!({}))
        .await
        .expect("status");
    assert_eq!(status["principal"], "zc_alice");
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn a_pooled_connection_cannot_be_rebound_to_keep_a_revoked_bearer_working() {
    let (_tmp, ctx, cancel, core, dials) = real_pool();
    let a = call_for(&core, "zc_real_a").await;
    assert!(
        a.request(
            Method::Initialize,
            json!({ "protocol_version": 1, "auth_token": "zc_real_b", "auth_provider": "native" }),
        )
        .await
        .is_err(),
        "a request must not re-initialize its connection"
    );
    assert!(ctx.auth.pairing().revoke_token("zc_real_a"));

    // A's key must still name a connection authenticated by A, so the
    // revocation binds: the next request with A is refused.
    let after = match core.access(&headers(Some("zc_real_a"), None)).await {
        Ok(CoreAccess::Core(call)) => call.request(Method::Status, json!({})).await,
        Ok(CoreAccess::InProcess) => panic!("served in-process"),
        Err(error) => Err(error),
    };
    assert!(
        matches!(&after, Err(CoreError::AuthRequired(_))),
        "the revoked bearer kept working: {after:?}"
    );
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    cancel.cancel();
}

/// A core that authenticates `zc_alice`, answers the next request with one
/// error, and closes the connection straight after it.
struct RefuseThenClose(i32);

impl Dial for RefuseThenClose {
    fn dial(&self) -> DialFuture<'_> {
        let code = self.0;
        Box::pin(async move {
            let (client, server) = tokio::io::duplex(64 * 1024);
            zeroclaw_spawn::spawn!(async move {
                let (read, mut write) = tokio::io::split(server);
                let mut lines = BufReader::new(read).lines();
                let Ok(Some(line)) = lines.next_line().await else {
                    return;
                };
                let init: Value = serde_json::from_str(&line).expect("initialize");
                assert_eq!(init["params"]["auth_token"], "zc_alice");
                let accepted = reply(
                    &init["id"],
                    json!({ "protocol_version": 1, "server_version": "fake", "server_pid": 1 }),
                );
                let _ = write.write_all(accepted.as_bytes()).await;
                let Ok(Some(line)) = lines.next_line().await else {
                    return;
                };
                let request: Value = serde_json::from_str(&line).expect("request");
                let refusal = refuse(&request["id"], code, "refused");
                let _ = write.write_all(refusal.as_bytes()).await;
                let _ = write.shutdown().await;
            });
            Some(client)
        })
    }
}

#[tokio::test]
async fn a_core_reply_stands_when_the_connection_closes_right_after_it() {
    for (code, expected) in [
        (AUTH_REQUIRED, (StatusCode::UNAUTHORIZED, "auth_required")),
        (FORBIDDEN, (StatusCode::FORBIDDEN, "forbidden")),
    ] {
        let core = CoreRpc::with_dialer(RefuseThenClose(code), || true, PoolLimits::default());
        let call = call_for(&core, "zc_alice").await;
        let error = call
            .request(Method::Status, json!({}))
            .await
            .expect_err("the core refused");
        assert_eq!(status_of(&error), expected, "code {code}: {error:?}");
        assert_eq!(pool_of(&core).pooled(), 0, "code {code}");
    }
}

#[tokio::test(start_paused = true)]
async fn requests_holding_connections_count_against_the_cap() {
    let fake = FakeCore::accepting(&["zc_a", "zc_b", "zc_c"]);
    let limits = PoolLimits {
        max_credentials: 1,
        capacity_wait: Duration::from_millis(200),
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    let pool = pool_of(&core);

    let a = call_for(&core, "zc_a").await;
    connection_of(&a).await;
    for token in ["zc_b", "zc_c"] {
        match core.access(&headers(Some(token), None)).await {
            Err(error) => assert_eq!(
                status_of(&error),
                (StatusCode::SERVICE_UNAVAILABLE, "core_busy"),
                "{token}"
            ),
            Ok(_) => panic!("{token}: admitted past the cap"),
        }
    }
    assert_eq!(pool.open_connections(), 1);
    assert_eq!(lock(&fake.open).len(), 1);
    assert_eq!(fake.dials(), 1, "a refused request never dials");

    // Once a's request lets go, its idle connection makes room for b.
    let a_connection = connection_of(&a).await as usize;
    drop(a);
    let b = call_for(&core, "zc_b").await;
    connection_of(&b).await;
    wait_until("a's connection closes", || !fake.is_open(a_connection)).await;
    assert_eq!(pool.open_connections(), 1);
}

/// A dialer whose connection never arrives.
struct HangingDial;

impl Dial for HangingDial {
    fn dial(&self) -> DialFuture<'_> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn a_waiter_takes_the_capacity_a_request_lets_go_of() {
    let fake = FakeCore::accepting(&["zc_a", "zc_b"]);
    let limits = PoolLimits {
        max_credentials: 1,
        capacity_wait: Duration::from_millis(200),
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    let a = call_for(&core, "zc_a").await;
    connection_of(&a).await;

    let waiter = {
        let core = core.clone();
        zeroclaw_spawn::spawn!(async move {
            core.access(&headers(Some("zc_b"), None)).await.map(|_| ())
        })
    };
    // b is now waiting for capacity that a holds. a's request ends well
    // before b's deadline; its idle connection must make room for b.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(a);
    assert!(
        matches!(waiter.await.expect("join"), Ok(())),
        "the waiter was not admitted when a's connection went idle"
    );
    assert_eq!(fake.dials(), 2);
    assert_eq!(pool_of(&core).open_connections(), 1);
}

type ReserveFuture = Pin<Box<dyn Future<Output = Result<OwnedSemaphorePermit, CoreError>> + Send>>;

/// A waker that polls the future it wakes on the spot, inside the waking
/// call. A waiter on another runtime worker can run at exactly that moment;
/// a current-thread test cannot otherwise reach it.
struct EagerWaiter {
    future: Mutex<Option<ReserveFuture>>,
    outcome: Mutex<Option<Result<OwnedSemaphorePermit, CoreError>>>,
    polls: AtomicUsize,
}

impl EagerWaiter {
    fn poll_now(self: &Arc<Self>) {
        let waker = std::task::Waker::from(Arc::clone(self));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut future = lock(&self.future);
        if let Some(pending) = future.as_mut() {
            self.polls.fetch_add(1, Ordering::SeqCst);
            if let std::task::Poll::Ready(outcome) = pending.as_mut().poll(&mut cx) {
                *future = None;
                *lock(&self.outcome) = Some(outcome);
            }
        }
    }
}

impl std::task::Wake for EagerWaiter {
    fn wake(self: Arc<Self>) {
        self.poll_now();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.poll_now();
    }
}

#[tokio::test(start_paused = true)]
async fn a_waiter_woken_by_a_release_finds_the_connection_evictable() {
    let fake = FakeCore::accepting(&["zc_a", "zc_b"]);
    let limits = PoolLimits {
        max_credentials: 1,
        capacity_wait: Duration::from_millis(200),
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    let a = call_for(&core, "zc_a").await;
    connection_of(&a).await;

    let pool = pool_of(&core);
    let b_key = HttpCredential {
        provider: NATIVE_PROVIDER,
        token: "zc_b",
    }
    .key();
    let waiter = Arc::new(EagerWaiter {
        future: Mutex::new(Some(Box::pin(async move { pool.reserve(&b_key).await }))),
        outcome: Mutex::new(None),
        polls: AtomicUsize::new(0),
    });
    waiter.poll_now();
    assert!(
        lock(&waiter.outcome).is_none(),
        "a still holds the capacity"
    );

    // Dropping a's request wakes the waiter, which re-checks at once. By
    // then a's hold on the connection must already be gone.
    let polls = waiter.polls.load(Ordering::SeqCst);
    drop(a);
    assert!(
        waiter.polls.load(Ordering::SeqCst) > polls,
        "the release woke the waiter"
    );
    tokio::time::advance(Duration::from_millis(201)).await;
    let outcome = lock(&waiter.outcome).take();
    assert!(
        matches!(outcome, Some(Ok(_))),
        "an idle connection existed before the deadline, but reserve answered {outcome:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_dial_in_progress_holds_capacity() {
    let limits = PoolLimits {
        max_credentials: 1,
        capacity_wait: Duration::from_millis(200),
        ..PoolLimits::default()
    };
    let core = CoreRpc::with_dialer(HangingDial, || true, limits);
    let pool = pool_of(&core);

    let first = {
        let core = core.clone();
        zeroclaw_spawn::spawn!(async move {
            core.access(&headers(Some("zc_a"), None)).await.map(|_| ())
        })
    };
    wait_until("the first dial holds the capacity", || {
        pool.open_connections() == 1
    })
    .await;
    match core.access(&headers(Some("zc_b"), None)).await {
        Err(error) => assert_eq!(
            status_of(&error),
            (StatusCode::SERVICE_UNAVAILABLE, "core_busy")
        ),
        Ok(_) => panic!("a second dial was admitted past the cap"),
    }
    // The hung dial gives up and returns its capacity.
    assert!(matches!(
        first.await.expect("join"),
        Err(CoreError::Unavailable(_))
    ));
    assert_eq!(pool.open_connections(), 0);
}

#[tokio::test]
async fn a_connection_the_core_closed_gives_its_capacity_back() {
    let fake = FakeCore::accepting(&["zc_a", "zc_b"]);
    let limits = PoolLimits {
        max_credentials: 1,
        capacity_wait: Duration::from_millis(50),
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    {
        let a = call_for(&core, "zc_a").await;
        connection_of(&a).await;
    }
    fake.drop_connections.notify_waiters();
    wait_until("the core drops a", || lock(&fake.open).is_empty()).await;

    let b = call_for(&core, "zc_b").await;
    connection_of(&b).await;
    assert_eq!(pool_of(&core).open_connections(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_request_that_times_out_leaves_the_pool() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let alice = call_for(&core, "zc_alice").await;

    let error = alice
        .request(Method::Health, json!({}))
        .await
        .expect_err("the fake core never answers health");
    assert!(matches!(error, CoreError::Timeout), "{error:?}");
    assert_eq!(pool_of(&core).pooled(), 0);

    // The next request dials again with the same credential.
    let again = call_for(&core, "zc_alice").await;
    connection_of(&again).await;
    assert_eq!(fake.handshake_tokens(), ["zc_alice", "zc_alice"]);
}

#[tokio::test(start_paused = true)]
async fn the_sweep_leaves_a_connection_a_request_still_holds() {
    let fake = FakeCore::accepting(&["zc_alice"]);
    let core = core_over(&fake, true, PoolLimits::default());
    let pool = pool_of(&core);
    let alice = call_for(&core, "zc_alice").await;
    connection_of(&alice).await;

    tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
    pool.sweep_idle();
    assert_eq!(pool.pooled(), 1, "a held connection is in use");

    drop(alice);
    pool.sweep_idle();
    assert_eq!(pool.pooled(), 0);
}

#[tokio::test]
async fn invalid_headers_cannot_reach_a_warm_or_evicted_connection() {
    let fake = FakeCore::accepting(&["zc_a", "zc_b"]);
    let limits = PoolLimits {
        max_credentials: 1,
        ..PoolLimits::default()
    };
    let core = core_over(&fake, true, limits);
    for token in ["zc_a", "zc_b"] {
        let call = call_for(&core, token).await;
        connection_of(&call).await;
    }
    let dials = fake.dials();
    for bad in [
        HeaderMap::new(),
        headers(Some(""), None),
        headers(Some("  "), None),
        headers(Some("zc_a"), Some("peercred")),
        headers(Some("zc_a"), Some("")),
    ] {
        assert!(matches!(
            core.access(&bad).await,
            Err(CoreError::AuthRequired(_))
        ));
    }
    assert_eq!(fake.dials(), dials);
    fake.assert_no_credential_less_handshake();
}

#[tokio::test]
async fn a_bearer_refusal_names_what_the_selected_provider_expects() {
    let core = core_over(&FakeCore::accepting(&[]), true, PoolLimits::default());
    let message = |error: CoreError| match error {
        CoreError::AuthRequired(message) => message,
        other => panic!("{other:?}"),
    };
    let native = core.access(&headers(Some(""), None)).await;
    assert_eq!(message(native.err().expect("refused")), PAIR_FIRST_MESSAGE);
    let oidc = core.access(&headers(None, Some("oidc.corp"))).await;
    assert_eq!(message(oidc.err().expect("refused")), OIDC_BEARER_MESSAGE);
}

#[tokio::test]
async fn an_unknown_oidc_alias_is_the_cores_call_and_is_never_pooled() {
    // The gateway checks provider syntax only; whether `oidc.<alias>` is
    // configured is decided by the core, on one dial that presents the
    // credential. The refusal is a 401 and nothing stays pooled.
    let (_tmp, _ctx, cancel, core, dials) = real_pool();
    let error = match core
        .access(&headers(Some("zc_real_a"), Some("oidc.not-configured")))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("an unknown provider was accepted"),
    };
    assert_eq!(
        status_of(&error),
        (StatusCode::UNAUTHORIZED, "auth_required")
    );
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    assert_eq!(pool_of(&core).pooled(), 0);
    assert_eq!(pool_of(&core).open_connections(), 0);
    cancel.cancel();
}

// ── Subscriptions ────────────────────────────────────────────────

/// What a [`ProxyDial`] does to the traffic between the pool and the core.
#[derive(Default)]
struct Faults {
    /// Hold the core's reply to the next request with this method.
    hold: Mutex<Option<&'static str>>,
    /// The subscription id in the reply being held.
    held: Mutex<Option<String>>,
    held_ready: Notify,
    release: Notify,
    /// Answer every `subscription/cancel` with a refusal, never reaching the
    /// core.
    refuse_cancel: AtomicBool,
    /// The ids of the `subscription/cancel` requests the core answered.
    cancelled: Mutex<Vec<String>>,
    /// The pool closed its side of a connection.
    closed_by_gateway: AtomicBool,
}

/// A dialer that puts a line proxy between the pool and the real core, so a
/// test can hold a reply the core has already sent, or refuse a cancel.
struct ProxyDial {
    connector: InprocConnector,
    faults: Arc<Faults>,
    dials: Arc<AtomicUsize>,
}

impl Dial for ProxyDial {
    fn dial(&self) -> DialFuture<'_> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        let faults = Arc::clone(&self.faults);
        Box::pin(async move {
            let core = self.connector.dial().await?;
            let (gateway, proxy) = tokio::io::duplex(64 * 1024);
            zeroclaw_spawn::spawn!(relay(proxy, core, faults));
            Some(gateway)
        })
    }
}

async fn relay(gateway: DuplexStream, core: DuplexStream, faults: Arc<Faults>) {
    let (from_gateway, to_gateway) = tokio::io::split(gateway);
    let (from_core, mut to_core) = tokio::io::split(core);
    let to_gateway = Arc::new(tokio::sync::Mutex::new(to_gateway));
    let held_ids: Arc<Mutex<HashSet<String>>> = Arc::default();
    let cancels: Arc<Mutex<HashMap<String, String>>> = Arc::default();

    let upstream = {
        let (faults, to_gateway, held_ids, cancels) = (
            Arc::clone(&faults),
            Arc::clone(&to_gateway),
            Arc::clone(&held_ids),
            Arc::clone(&cancels),
        );
        async move {
            let mut lines = BufReader::new(from_gateway).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let message: Value = serde_json::from_str(&line).unwrap_or_default();
                let id = message["id"].to_string();
                let method = message["method"].as_str();
                if method == Some("subscription/cancel") {
                    if faults.refuse_cancel.load(Ordering::SeqCst) {
                        let refusal = json!({
                            "jsonrpc": "2.0",
                            "id": message["id"],
                            "error": { "code": FORBIDDEN, "message": "refused by the test" },
                        });
                        let mut out = to_gateway.lock().await;
                        let _ = out.write_all(format!("{refusal}\n").as_bytes()).await;
                        continue;
                    }
                    let target = message["params"]["subscription_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    lock(&cancels).insert(id.clone(), target);
                }
                let hold = *lock(&faults.hold);
                if method.is_some() && method == hold {
                    lock(&held_ids).insert(id);
                    *lock(&faults.hold) = None;
                }
                if to_core
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            faults.closed_by_gateway.store(true, Ordering::SeqCst);
            let _ = to_core.shutdown().await;
        }
    };
    let downstream = async move {
        let mut lines = BufReader::new(from_core).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let message: Value = serde_json::from_str(&line).unwrap_or_default();
            let id = message["id"].to_string();
            if message.get("method").is_none() {
                if lock(&held_ids).remove(&id) {
                    *lock(&faults.held) = message["result"]["subscription_id"]
                        .as_str()
                        .map(str::to_owned);
                    faults.held_ready.notify_one();
                    faults.release.notified().await;
                }
                if let Some(target) = lock(&cancels).remove(&id) {
                    lock(&faults.cancelled).push(target);
                }
            }
            let mut out = to_gateway.lock().await;
            if out.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    };
    tokio::join!(upstream, downstream);
}

/// A real in-process core that streams events, behind a [`ProxyDial`], with
/// one paired native bearer.
fn proxied_core() -> (
    tempfile::TempDir,
    Arc<zeroclaw_runtime::rpc::context::RpcContext>,
    tokio_util::sync::CancellationToken,
    InprocConnector,
    CoreRpc,
    Arc<Faults>,
    Arc<AtomicUsize>,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = zeroclaw_config::schema::Config {
        data_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config.gateway.require_pairing = true;
    config.gateway.paired_tokens = vec!["zc_sub".into()];
    with_daemon_signing_key(&config);
    let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
        16,
        Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        )),
    ));
    let mut ctx = zeroclaw_runtime::rpc::context::RpcContext::for_live_test(config, sessions);
    Arc::get_mut(&mut ctx)
        .expect("a fresh context has one owner")
        .event_tx = Some(tokio::sync::broadcast::channel(64).0);
    let cancel = tokio_util::sync::CancellationToken::new();
    let connector = InprocConnector::new(cancel.clone());
    connector.bind(Arc::clone(&ctx));
    let faults = Arc::new(Faults::default());
    let dials = Arc::new(AtomicUsize::new(0));
    let core = CoreRpc::with_dialer(
        ProxyDial {
            connector: connector.clone(),
            faults: Arc::clone(&faults),
            dials: Arc::clone(&dials),
        },
        || true,
        PoolLimits::default(),
    );
    (tmp, ctx, cancel, connector, core, faults, dials)
}

/// Publish one log frame carrying `probe`.
fn publish(ctx: &zeroclaw_runtime::rpc::context::RpcContext, probe: &str) {
    ctx.subscriptions.publish(
        zeroclaw_runtime::rpc::subscription::Source::Logs,
        json!({ "type": "log", "message": probe }),
    );
}

/// Publish a frame and wait until `live` receives it; return every frame of
/// `abandoned` that arrived on the connection meanwhile.
async fn frames_for(
    ctx: &zeroclaw_runtime::rpc::context::RpcContext,
    live: &mut CoreSubscription<Value>,
    abandoned: &str,
    probe: &str,
) -> Vec<Value> {
    publish(ctx, probe);
    let mut leaked = Vec::new();
    let reached = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Ok(note) = live.notifications.recv().await else {
                continue;
            };
            if note.params["subscription_id"] == abandoned {
                leaked.push(note.params.clone());
            }
            if live.is_mine(&note) && note.params["message"] == probe {
                return;
            }
        }
    })
    .await;
    assert!(reached.is_ok(), "the live subscription never saw {probe}");
    // Give a frame for the abandoned subscription, if any, a moment more.
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(note) = live.notifications.try_recv() {
        if note.params["subscription_id"] == abandoned {
            leaked.push(note.params);
        }
    }
    leaked
}

async fn wait_for_cancel(faults: &Faults, id: &str) {
    wait_until(&format!("the core cancelling {id}"), || {
        lock(&faults.cancelled)
            .iter()
            .any(|cancelled| cancelled == id)
    })
    .await;
}

/// The core opens the subscription and replies, but the caller has stopped
/// waiting by the time the reply arrives. The late subscription is cancelled
/// and its connection stays in use, carrying nothing for it.
#[tokio::test]
async fn a_caller_that_stops_waiting_leaves_no_subscription_behind() {
    let (_tmp, ctx, cancel, _connector, core, faults, dials) = proxied_core();
    *lock(&faults.hold) = Some("logs/subscribe");
    let caller = call_for(&core, "zc_sub").await;
    let waiting = zeroclaw_spawn::spawn!(async move {
        caller
            .subscribe::<Value>(Method::LogsSubscribe, json!({}))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), faults.held_ready.notified())
        .await
        .expect("the core replied");
    let abandoned = lock(&faults.held).clone().expect("the reply names it");
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    faults.release.notify_one();

    // The late subscription is cancelled; the connection stays pooled and
    // in use.
    wait_for_cancel(&faults, &abandoned).await;
    let mut live = call_for(&core, "zc_sub")
        .await
        .subscribe::<Value>(Method::LogsSubscribe, json!({}))
        .await
        .expect("a live subscription on the same connection");
    assert_eq!(dials.load(Ordering::SeqCst), 1, "the same connection");
    let leaked = frames_for(&ctx, &mut live, &abandoned, "after-abandon").await;
    assert!(
        leaked.is_empty(),
        "the abandoned subscription delivered {leaked:?}"
    );
    assert!(!faults.closed_by_gateway.load(Ordering::SeqCst));
    cancel.cancel();
}

/// `/api/events` through the core opens its subscription with
/// [`CoreCall::subscribe`]: a client that leaves while the core's reply is
/// in flight leaves nothing behind, though another request keeps the same
/// connection open.
#[tokio::test]
async fn the_event_stream_abandoned_during_setup_leaves_no_subscription() {
    let (_tmp, _ctx, cancel, _connector, core, faults, dials) = proxied_core();
    let keeper = call_for(&core, "zc_sub").await;
    *lock(&faults.hold) = Some("logs/subscribe");
    let opening = zeroclaw_spawn::spawn!(crate::sse::events_stream_through_core(
        call_for(&core, "zc_sub").await
    ));
    tokio::time::timeout(Duration::from_secs(5), faults.held_ready.notified())
        .await
        .expect("the core replied");
    let abandoned = lock(&faults.held).clone().expect("the reply names it");
    opening.abort();
    assert!(opening.await.unwrap_err().is_cancelled());
    faults.release.notify_one();

    wait_for_cancel(&faults, &abandoned).await;
    assert_eq!(dials.load(Ordering::SeqCst), 1, "the same connection");
    let again = keeper
        .request(
            Method::SubscriptionCancel,
            json!({ "subscription_id": abandoned }),
        )
        .await
        .expect("the connection still serves");
    assert_eq!(again["cancelled"], json!(false), "nothing left to cancel");
    assert!(!faults.closed_by_gateway.load(Ordering::SeqCst));
    cancel.cancel();
}

/// The caller held the subscription and dropped it at once, as a failed
/// upgrade does: it is cancelled.
#[tokio::test]
async fn a_subscription_dropped_after_it_opened_is_cancelled() {
    let (_tmp, ctx, cancel, _connector, core, faults, _dials) = proxied_core();
    let dropped = call_for(&core, "zc_sub")
        .await
        .subscribe::<Value>(Method::LogsSubscribe, json!({}))
        .await
        .expect("subscribed");
    let id = dropped.id().to_owned();
    assert_eq!(dropped.opened["subscription_id"], id);
    drop(dropped);

    wait_for_cancel(&faults, &id).await;
    let mut live = call_for(&core, "zc_sub")
        .await
        .subscribe::<Value>(Method::LogsSubscribe, json!({}))
        .await
        .expect("a live subscription");
    let leaked = frames_for(&ctx, &mut live, &id, "after-drop").await;
    assert!(
        leaked.is_empty(),
        "the dropped subscription delivered {leaked:?}"
    );
    cancel.cancel();
}

/// A cancel the core refuses leaves a subscription that cannot be ended by
/// id. Another request still holds the connection, which alone would keep
/// it (and the subscription) open, so the connection is retired: closed and
/// out of the pool, and the next request dials again.
#[tokio::test]
async fn a_refused_cancel_retires_the_connection() {
    let (_tmp, _ctx, cancel, connector, core, faults, dials) = proxied_core();
    faults.refuse_cancel.store(true, Ordering::SeqCst);
    let subscription = call_for(&core, "zc_sub")
        .await
        .subscribe::<Value>(Method::LogsSubscribe, json!({}))
        .await
        .expect("subscribed");
    let keeper = call_for(&core, "zc_sub").await;
    assert_eq!(dials.load(Ordering::SeqCst), 1, "both on one connection");
    assert!(connector.connection_count() > 0);
    drop(subscription);

    wait_until("the gateway closing the connection", || {
        faults.closed_by_gateway.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        keeper.request(Method::Status, json!({})).await.is_err(),
        "the retired connection serves nothing more"
    );
    drop(keeper);
    wait_until("the core seeing it close", || {
        connector.connection_count() == 0
    })
    .await;
    call_for(&core, "zc_sub")
        .await
        .request(Method::Status, json!({}))
        .await
        .expect("the next request");
    assert_eq!(dials.load(Ordering::SeqCst), 2, "a fresh connection");
    cancel.cancel();
}

/// A refused subscribe opens nothing and keeps the connection.
#[tokio::test]
async fn a_refused_subscribe_opens_nothing() {
    let (_tmp, _ctx, cancel, _connector, core, faults, dials) = proxied_core();
    let refused = call_for(&core, "zc_sub")
        .await
        .subscribe::<Value>(
            Method::LogsSubscribe,
            json!({ "since_seq": "not a number" }),
        )
        .await
        .expect_err("refused");
    assert_eq!(status_of(&refused).0, StatusCode::BAD_REQUEST);
    call_for(&core, "zc_sub")
        .await
        .request(Method::Status, json!({}))
        .await
        .expect("the connection still serves");
    assert_eq!(dials.load(Ordering::SeqCst), 1);
    assert!(!faults.closed_by_gateway.load(Ordering::SeqCst));
    cancel.cancel();
}
