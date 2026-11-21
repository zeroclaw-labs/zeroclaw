//! Local transport conformance.
//!
//! One scenario list, run against the daemon's real listener once per
//! [`Leg`]. The local leg is a Unix domain socket on Unix and a named pipe
//! on Windows. Every scenario speaks NDJSON JSON-RPC the way an external
//! client does, so the same assertions hold on every transport. Only
//! opening a leg's stream is platform-specific; each scenario compiles on
//! every target.
//!
//! Covered here: the handshake (initialize, refusal before it, protocol
//! version), identity and denial (a scoped principal held to its grants),
//! endpoint discovery, turn streaming (updates delivered while the turn
//! runs; completed, failed and cancelled turns; a closed connection), and
//! subscriptions (a replay cursor that expired, and a live subscriber that
//! stops reading). Frame bounds, the connection ceiling and the initialize
//! deadline have per-transport tests beside the listener in `local.rs`.
//! Scenarios that need a transport-intrinsic peer identity are Unix-only: a
//! named pipe carries no peer uid.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use futures_util::stream::{self, BoxStream};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::jsonrpc::error_codes::{
    AUTH_REQUIRED, FORBIDDEN, SESSION_NOT_FOUND, VERSION_MISMATCH,
};
use zeroclaw_api::model_provider::{
    ChatRequest, ModelProvider, StreamChunk, StreamError, StreamEvent, StreamOptions, StreamResult,
};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_queue::SessionActorQueue;

use super::context::RpcContext;
use super::dispatch::{Method, RPC_PROTOCOL_VERSION};
use super::local::{run_local_listener, socket_path};
use super::session::SessionStore;

/// Upper bound on any single wait for the daemon.
const WAIT: Duration = Duration::from_secs(10);

/// A transport the daemon serves clients over. Every cross-platform
/// scenario runs once per leg, so another transport joins the suite by
/// adding a leg rather than a copy of each scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leg {
    /// The daemon's local endpoint: a Unix domain socket on Unix, a named
    /// pipe on Windows.
    Local,
}

impl Leg {
    const ALL: [Leg; 1] = [Leg::Local];

    /// A client stream to the daemon listening at `endpoint` on this leg.
    async fn open(self, endpoint: &Path) -> (ReadHalf, WriteHalf) {
        match self {
            Self::Local => open_local(endpoint).await,
        }
    }
}

impl std::fmt::Display for Leg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local if cfg!(windows) => "named pipe",
            Self::Local => "unix socket",
        })
    }
}

type ReadHalf = Box<dyn AsyncRead + Send + Unpin>;
type WriteHalf = Box<dyn AsyncWrite + Send + Unpin>;

/// Connect to the daemon's endpoint, retrying while the listener comes up.
#[cfg(unix)]
async fn open_local(endpoint: &Path) -> (ReadHalf, WriteHalf) {
    for _ in 0..250 {
        if let Ok(stream) = tokio::net::UnixStream::connect(endpoint).await {
            let (read, write) = stream.into_split();
            return (Box::new(read), Box::new(write));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no daemon accepted on {}", endpoint.display());
}

/// Connect to the daemon's endpoint, retrying while the listener creates its
/// pending pipe instance.
#[cfg(windows)]
async fn open_local(endpoint: &Path) -> (ReadHalf, WriteHalf) {
    use tokio::net::windows::named_pipe::ClientOptions;
    let name = endpoint.to_string_lossy().into_owned();
    for _ in 0..250 {
        if let Ok(client) = ClientOptions::new().open(&name) {
            let (read, write) = tokio::io::split(client);
            return (Box::new(read), Box::new(write));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no daemon accepted on {name}");
}

/// A daemon listening on its real endpoint for one leg.
struct Daemon {
    leg: Leg,
    ctx: Arc<RpcContext>,
    endpoint: PathBuf,
    clients: Arc<AtomicUsize>,
    cancel: CancellationToken,
    listener: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    /// Serve `ctx` on `leg`, at the endpoint its configuration resolves to.
    fn start(leg: Leg, ctx: Arc<RpcContext>) -> Self {
        // With the override set, every scenario would resolve one shared
        // endpoint instead of its own data directory's.
        assert!(
            std::env::var_os("ZEROCLAW_SOCKET").is_none(),
            "ZEROCLAW_SOCKET must be unset for the conformance scenarios"
        );
        let endpoint = socket_path(&ctx.config.read());
        let clients = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let listener = {
            let ctx = Arc::clone(&ctx);
            let clients = Arc::clone(&clients);
            let cancel = cancel.clone();
            zeroclaw_spawn::spawn!(
                async move { run_local_listener(ctx, cancel, clients, None).await }
            )
        };
        Self {
            leg,
            ctx,
            endpoint,
            clients,
            cancel,
            listener,
        }
    }

    async fn connect(&self) -> Client {
        let (read, write) = self.leg.open(&self.endpoint).await;
        Client {
            leg: self.leg,
            reader: BufReader::new(read),
            writer: write,
            next_id: 0,
            responses: HashMap::new(),
            notifications: VecDeque::new(),
        }
    }

    /// Wait until the listener counts exactly `expected` live connections.
    async fn wait_for_clients(&self, expected: usize) {
        let reached = tokio::time::timeout(WAIT, async {
            while self.clients.load(Ordering::Relaxed) != expected {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            reached.is_ok(),
            "{}: client count never reached {expected}; last observed {}",
            self.leg,
            self.clients.load(Ordering::Relaxed)
        );
    }

    /// Stop the listener; a panic or an error from it fails the scenario.
    async fn stop(self) {
        let leg = self.leg;
        self.cancel.cancel();
        let stopped = tokio::time::timeout(WAIT, self.listener)
            .await
            .unwrap_or_else(|_| panic!("{leg}: the listener did not stop"));
        let result = stopped.unwrap_or_else(|e| panic!("{leg}: the listener panicked: {e}"));
        if let Err(error) = result {
            panic!("{leg}: the listener failed: {error:#}");
        }
    }
}

/// An NDJSON JSON-RPC client over one connection. Responses are matched to
/// their requests by id in whatever order they arrive; notifications queue
/// in arrival order.
struct Client {
    leg: Leg,
    reader: BufReader<ReadHalf>,
    writer: WriteHalf,
    next_id: u64,
    responses: HashMap<u64, Value>,
    notifications: VecDeque<Value>,
}

/// One request's response.
struct Exchange {
    leg: Leg,
    response: Value,
}

impl Exchange {
    fn result(&self) -> &Value {
        assert!(
            self.response["error"].is_null(),
            "{}: unexpected RPC error: {}",
            self.leg,
            self.response
        );
        &self.response["result"]
    }

    fn error_code(&self) -> i64 {
        self.response["error"]["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("{}: expected an RPC error: {}", self.leg, self.response))
    }
}

impl Client {
    /// Send `method` with `params`; returns the request id.
    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let mut line = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        })
        .to_string();
        line.push('\n');
        let leg = self.leg;
        self.writer
            .write_all(line.as_bytes())
            .await
            .unwrap_or_else(|e| panic!("{leg}: write {method}: {e}"));
        id
    }

    /// Read one frame from the daemon and file it. Fails on end of stream
    /// or silence.
    async fn read_frame(&mut self) {
        let leg = self.leg;
        let mut line = String::new();
        let read = tokio::time::timeout(WAIT, self.reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("{leg}: no frame within {WAIT:?}"))
            .unwrap_or_else(|e| panic!("{leg}: read failed: {e}"));
        assert_ne!(read, 0, "{leg}: the daemon closed the connection");
        let frame: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("{leg}: frame is not JSON ({e}): {line}"));
        if frame.get("method").is_some() && frame.get("id").is_none() {
            self.notifications.push_back(frame);
        } else if let Some(id) = frame["id"].as_u64() {
            self.responses.insert(id, frame);
        } else {
            panic!("{leg}: frame answers no request: {frame}");
        }
    }

    /// The response to request `id`.
    async fn response(&mut self, id: u64) -> Exchange {
        loop {
            if let Some(response) = self.responses.remove(&id) {
                return Exchange {
                    leg: self.leg,
                    response,
                };
            }
            self.read_frame().await;
        }
    }

    /// The next notification, in arrival order.
    async fn notification(&mut self) -> Value {
        loop {
            if let Some(frame) = self.notifications.pop_front() {
                return frame;
            }
            self.read_frame().await;
        }
    }

    async fn call(&mut self, method: Method, params: Value) -> Exchange {
        let id = self.send(method.wire_name(), params).await;
        self.response(id).await
    }

    /// `initialize` with protocol version 1 and no explicit credential.
    async fn initialize(&mut self) -> Exchange {
        self.call(
            Method::Initialize,
            json!({ "protocol_version": RPC_PROTOCOL_VERSION }),
        )
        .await
    }

    /// Answer a request so every frame the daemon sent before its response
    /// has been read: afterwards the notification queue holds everything
    /// that preceded it.
    async fn barrier(&mut self) {
        self.call(Method::Health, json!({})).await.result();
    }

    /// Read notifications until `session_id`'s terminal event, returning
    /// every `session/update` for that session in arrival order.
    async fn updates_until_turn_complete(&mut self, session_id: &str) -> Vec<Value> {
        let mut updates = Vec::new();
        loop {
            let frame = self.notification().await;
            if frame["method"] != "session/update" || frame["params"]["session_id"] != session_id {
                continue;
            }
            let terminal = frame["params"]["type"] == "turn_complete";
            updates.push(frame["params"].clone());
            if terminal {
                return updates;
            }
        }
    }

    /// Count the `turn_complete` events for `session_id` still queued.
    fn queued_terminals(&mut self, session_id: &str) -> usize {
        self.notifications
            .drain(..)
            .filter(|frame| {
                frame["method"] == "session/update"
                    && frame["params"]["session_id"] == session_id
                    && frame["params"]["type"] == "turn_complete"
            })
            .count()
    }
}

fn config_in(dir: &Path) -> Config {
    Config {
        data_dir: dir.to_path_buf(),
        config_path: dir.join("config.toml"),
        ..Config::default()
    }
}

fn sessions() -> Arc<SessionStore> {
    let queue = Arc::new(SessionActorQueue::new(4, 30, 60));
    Arc::new(SessionStore::new(64, queue))
}

fn daemon_with(leg: Leg, config: Config) -> Daemon {
    Daemon::start(leg, RpcContext::minimal(config, sessions()))
}

/// A permission profile that grants only `system:read`.
fn status_reader_profile() -> zeroclaw_config::schema::PermissionProfileConfig {
    use zeroclaw_api::grants::{Resource, Verb};
    zeroclaw_config::schema::PermissionProfileConfig {
        grants: HashMap::from([(Resource::System, vec![Verb::Read])]),
        ..Default::default()
    }
}

/// A roster of one user holding the status-reader profile, bound to `uid`.
/// The daemon's own uid is not trusted, so a local peer maps through the
/// roster or not at all.
fn roster_config(dir: &Path, uid: u32) -> Config {
    use zeroclaw_config::schema::UserConfig;
    let mut config = config_in(dir);
    config.security.trust_daemon_uid = false;
    config
        .permission_profiles
        .insert("status-reader".into(), status_reader_profile());
    config.users.insert(
        "alice".into(),
        UserConfig {
            principal_id: None,
            uid: Some(uid),
            permission_profiles: vec!["status-reader".into()],
        },
    );
    config
}

/// An OpenID provider that answers token introspection: every access token
/// is active, for `subject`, in `group`.
async fn introspection_idp(subject: &str, group: &str) -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
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
        .expect("clock after the epoch")
        .as_secs();
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "active": true,
            "token_type": "Bearer",
            "client_id": "conformance-client",
            "iss": issuer,
            "sub": subject,
            "aud": "zeroclaw",
            "exp": now + 600,
            "groups": [group],
        })))
        .mount(&server)
        .await;
    server
}

/// `oidc.corp` verifying tokens by introspection at `issuer`, mapping the
/// `viewers` group to the status-reader profile.
fn scoped_oidc_config(dir: &Path, issuer: &str) -> Config {
    use zeroclaw_config::schema::{OidcConfig, OidcValidation};
    let mut config = config_in(dir);
    config
        .permission_profiles
        .insert("status-reader".into(), status_reader_profile());
    config.oidc.insert(
        "corp".into(),
        OidcConfig {
            issuer: issuer.to_owned(),
            audience: "zeroclaw".into(),
            client_id: "daemon".into(),
            client_secret: Some("conformance-secret".into()),
            validation: OidcValidation::Introspection,
            claim_path: "groups".into(),
            profile_map: HashMap::from([("viewers".into(), "status-reader".into())]),
            interactive_clients: vec!["conformance-client".into()],
            ..OidcConfig::default()
        },
    );
    config
}

/// A provider that streams `alpha `, then waits for `release` before it
/// either streams `beta` and finishes or fails. Released stays released, so
/// a retried stream cannot park.
struct ScriptedStream {
    release: tokio::sync::watch::Receiver<bool>,
    fail_after_first_chunk: bool,
}

#[async_trait]
impl ModelProvider for ScriptedStream {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("the scripted provider only streams")
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamEvent>> {
        let mut release = self.release.clone();
        let fail = self.fail_after_first_chunk;
        let first = stream::iter([Ok(StreamEvent::TextDelta(StreamChunk::delta("alpha ")))]);
        let second = stream::once(async move {
            let _ = release.wait_for(|released| *released).await;
            if fail {
                Err(StreamError::ModelProvider(
                    "scripted provider failure".into(),
                ))
            } else {
                Ok(StreamEvent::TextDelta(StreamChunk::delta("beta")))
            }
        });
        let end = stream::iter(if fail {
            Vec::new()
        } else {
            vec![Ok(StreamEvent::Final)]
        });
        first.chain(second).chain(end).boxed()
    }
}

impl Attributable for ScriptedStream {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
    }

    fn alias(&self) -> &str {
        "conformance-scripted"
    }
}

/// A daemon with one session, `session_id`, served by a [`ScriptedStream`];
/// the returned sender releases the provider past its first chunk.
async fn scripted_daemon(
    leg: Leg,
    dir: &Path,
    session_id: &str,
    fail_after_first_chunk: bool,
) -> (Daemon, tokio::sync::watch::Sender<bool>) {
    use super::dispatch::connection_test_support::{fixture, insert_session};
    let fixture = fixture(dir).await;
    let (release, released) = tokio::sync::watch::channel(false);
    insert_session(
        &fixture.ctx,
        dir,
        session_id,
        Box::new(ScriptedStream {
            release: released,
            fail_after_first_chunk,
        }),
    )
    .await;
    (Daemon::start(leg, Arc::clone(&fixture.ctx)), release)
}

/// Prompt `session_id`, wait until its first chunk has reached the client,
/// then release the provider. Returns every update for the session through
/// its terminal event, and the prompt's response.
async fn run_scripted_turn(
    client: &mut Client,
    release: &tokio::sync::watch::Sender<bool>,
    session_id: &str,
) -> (Vec<Value>, Exchange) {
    let leg = client.leg;
    let prompt = client
        .send(
            Method::SessionPrompt.wire_name(),
            json!({
                "session_id": session_id,
                "prompt": "run",
                "client_turn_generation": 9,
            }),
        )
        .await;

    // The provider cannot go on until this chunk has crossed the transport,
    // so a transport that drops or holds back updates until the turn ends
    // never gets here.
    let mut updates = Vec::new();
    loop {
        let frame = client.notification().await;
        if frame["method"] != "session/update" || frame["params"]["session_id"] != session_id {
            continue;
        }
        let params = frame["params"].clone();
        assert_ne!(
            params["type"], "turn_complete",
            "{leg}: the turn ended before its first chunk arrived: {params}"
        );
        let first_chunk = params["type"] == "agent_message_chunk" && params["text"] == "alpha ";
        updates.push(params);
        if first_chunk {
            break;
        }
    }
    release.send_replace(true);

    updates.extend(client.updates_until_turn_complete(session_id).await);
    let response = client.response(prompt).await;
    (updates, response)
}

/// The text of every `agent_message_chunk`, in order.
fn chunk_texts(updates: &[Value]) -> Vec<&str> {
    updates
        .iter()
        .filter(|update| update["type"] == "agent_message_chunk")
        .map(|update| update["text"].as_str().expect("chunk text"))
        .collect()
}

// ── Handshake ────────────────────────────────────────────────────

#[tokio::test]
async fn initialize_binds_the_connection_and_advertises_every_method() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = daemon_with(leg, config_in(tmp.path()));
        let mut client = daemon.connect().await;

        let init = client.initialize().await;
        let result = init.result();
        assert_eq!(result["protocol_version"], RPC_PROTOCOL_VERSION);
        assert_eq!(result["server_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(result["server_pid"], std::process::id());
        assert!(result["tui_id"].is_string(), "{result}");
        // No roster: the endpoint's access control is the credential, and the
        // connection is the shared operator.
        assert_eq!(result["principal_id"], PrincipalId::SHARED_OPERATOR);

        let mut advertised: Vec<&str> = result["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .map(|name| name.as_str().expect("method name"))
            .collect();
        advertised.sort_unstable();
        let mut methods: Vec<&str> = Method::ALL.iter().map(|(_, name)| *name).collect();
        methods.sort_unstable();
        assert_eq!(
            advertised, methods,
            "{leg}: initialize advertises exactly the method table"
        );

        let status = client.call(Method::Status, json!({})).await;
        assert_eq!(status.result()["protocol_version"], RPC_PROTOCOL_VERSION);

        drop(client);
        daemon.stop().await;
    }
}

#[tokio::test]
async fn a_request_before_initialize_is_refused_and_initialize_still_succeeds() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = daemon_with(leg, config_in(tmp.path()));
        let mut client = daemon.connect().await;

        let early = client.call(Method::Status, json!({})).await;
        assert_eq!(early.error_code(), i64::from(AUTH_REQUIRED));

        client.initialize().await.result();
        client.call(Method::Status, json!({})).await.result();

        drop(client);
        daemon.stop().await;
    }
}

#[tokio::test]
async fn an_omitted_protocol_version_is_read_as_version_one() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = daemon_with(leg, config_in(tmp.path()));
        let mut client = daemon.connect().await;

        let init = client.call(Method::Initialize, json!({})).await;
        assert_eq!(init.result()["protocol_version"], 1);

        drop(client);
        daemon.stop().await;
    }
}

/// The handshake field is `protocol_version`. The camelCase `protocolVersion`
/// spelling is not a recognised field today: it is ignored, whatever value it
/// carries, and the handshake is read as version 1. This pins that behaviour
/// so that changing it is a deliberate protocol decision.
#[tokio::test]
async fn a_camel_case_protocol_version_is_ignored_and_read_as_version_one() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = daemon_with(leg, config_in(tmp.path()));

        for spelled in [1, RPC_PROTOCOL_VERSION + 1] {
            let mut client = daemon.connect().await;
            let init = client
                .call(Method::Initialize, json!({ "protocolVersion": spelled }))
                .await;
            assert_eq!(
                init.result()["protocol_version"],
                1,
                "{leg}: protocolVersion={spelled} is read as version 1"
            );
            drop(client);
        }

        daemon.stop().await;
    }
}

#[tokio::test]
async fn an_unsupported_protocol_version_is_refused_and_binds_nothing() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = daemon_with(leg, config_in(tmp.path()));
        let mut client = daemon.connect().await;

        for unsupported in [0, RPC_PROTOCOL_VERSION + 1] {
            let refused = client
                .call(
                    Method::Initialize,
                    json!({ "protocol_version": unsupported }),
                )
                .await;
            assert_eq!(refused.error_code(), i64::from(VERSION_MISMATCH));
            let message = refused.response["error"]["message"]
                .as_str()
                .expect("message");
            assert!(
                message.contains(&format!("server={RPC_PROTOCOL_VERSION}"))
                    && message.contains(&format!("client={unsupported}")),
                "{leg}: the refusal names both versions: {message}"
            );

            let unbound = client.call(Method::Status, json!({})).await;
            assert_eq!(
                unbound.error_code(),
                i64::from(AUTH_REQUIRED),
                "{leg}: a refused handshake binds no principal"
            );
        }

        // The same connection can still complete a supported handshake.
        client.initialize().await.result();
        client.call(Method::Status, json!({})).await.result();

        drop(client);
        daemon.stop().await;
    }
}

// ── Identity and denial ──────────────────────────────────────────

#[tokio::test]
async fn a_paired_token_binds_its_principal() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = config_in(tmp.path());
        config.gateway.paired_tokens = vec!["zc_conformance_paired".into()];
        let daemon = daemon_with(leg, config);
        let mut client = daemon.connect().await;

        let init = client
            .call(
                Method::Initialize,
                json!({
                    "protocol_version": RPC_PROTOCOL_VERSION,
                    "auth_token": "zc_conformance_paired",
                }),
            )
            .await;
        assert_eq!(init.result()["principal_id"], PrincipalId::SHARED_OPERATOR);
        client.call(Method::Status, json!({})).await.result();

        drop(client);
        daemon.stop().await;
    }
}

#[tokio::test]
async fn an_unpaired_token_is_refused_without_falling_back_to_the_endpoint() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = config_in(tmp.path());
        config.gateway.paired_tokens = vec!["zc_conformance_paired".into()];
        let daemon = daemon_with(leg, config);
        let mut client = daemon.connect().await;

        // Without a roster a tokenless local connection would be the shared
        // operator; a presented credential that fails must not reach that path.
        let refused = client
            .call(
                Method::Initialize,
                json!({
                    "protocol_version": RPC_PROTOCOL_VERSION,
                    "auth_token": "zc_conformance_never_paired",
                }),
            )
            .await;
        assert_eq!(refused.error_code(), i64::from(AUTH_REQUIRED));
        let unbound = client.call(Method::Status, json!({})).await;
        assert_eq!(unbound.error_code(), i64::from(AUTH_REQUIRED));

        drop(client);
        daemon.stop().await;
    }
}

/// An explicit OIDC bearer binds the scoped principal its provider resolves,
/// on any transport, and that principal's grants decide each call: a method
/// inside them is served, one outside them is refused with `FORBIDDEN`.
#[tokio::test]
async fn a_scoped_principal_is_held_to_its_grants() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let idp = introspection_idp("carol", "viewers").await;
        let daemon = daemon_with(leg, scoped_oidc_config(tmp.path(), &idp.uri()));
        let mut client = daemon.connect().await;

        let init = client
            .call(
                Method::Initialize,
                json!({
                    "protocol_version": RPC_PROTOCOL_VERSION,
                    "auth_token": "conformance-access-token",
                    "auth_provider": "oidc.corp",
                }),
            )
            .await;
        let principal = init.result()["principal_id"]
            .as_str()
            .expect("a bound principal")
            .to_owned();
        assert_ne!(
            principal,
            PrincipalId::SHARED_OPERATOR,
            "{leg}: the bearer binds its own principal"
        );

        client.call(Method::Status, json!({})).await.result();
        let denied = client.call(Method::ConfigGet, json!({})).await;
        assert_eq!(
            denied.error_code(),
            i64::from(FORBIDDEN),
            "{leg}: a method outside the principal's grants is refused"
        );
        // The refusal ends nothing: the connection keeps serving its grants.
        client.call(Method::Status, json!({})).await.result();

        drop(client);
        daemon.stop().await;
    }
}

/// One scenario, two refusal mechanisms. On a Unix socket the kernel peer
/// uid is a credential, and the roster does not name it. A named pipe
/// carries no peer uid, so the caller presents no credential at all, and
/// with a roster configured there is no local compatibility path to take.
#[tokio::test]
async fn with_a_roster_a_local_caller_it_does_not_name_is_refused() {
    for leg in Leg::ALL {
        use crate::security::auth_provider::PeercredAuthProvider;
        let tmp = tempfile::tempdir().unwrap();
        // A uid that is not this process: on Unix the kernel reports this
        // process's uid, which the roster does not name; a named pipe presents
        // no uid at all.
        let other_uid = PeercredAuthProvider::current_process_uid().wrapping_add(1);
        let daemon = daemon_with(leg, roster_config(tmp.path(), other_uid));
        let mut client = daemon.connect().await;

        let refused = client.initialize().await;
        assert_eq!(refused.error_code(), i64::from(AUTH_REQUIRED));
        let unbound = client.call(Method::Status, json!({})).await;
        assert_eq!(unbound.error_code(), i64::from(AUTH_REQUIRED));

        drop(client);
        daemon.stop().await;
    }
}

/// The socket's kernel-reported peer uid is a credential: it binds the
/// roster principal that names it, and that principal's grants decide each
/// call.
#[cfg(unix)]
#[tokio::test]
async fn the_peer_uid_binds_its_roster_principal_and_its_grants_gate_each_call() {
    // The kernel peer uid is a property of the local socket.
    let leg = Leg::Local;
    use crate::security::auth_provider::PeercredAuthProvider;
    let tmp = tempfile::tempdir().unwrap();
    let uid = PeercredAuthProvider::current_process_uid();
    let daemon = daemon_with(leg, roster_config(tmp.path(), uid));
    let mut client = daemon.connect().await;

    let init = client.initialize().await;
    assert_eq!(init.result()["principal_id"], "user:alice");

    client.call(Method::Status, json!({})).await.result();
    let denied = client.call(Method::ConfigGet, json!({})).await;
    assert_eq!(
        denied.error_code(),
        i64::from(FORBIDDEN),
        "a method outside the principal's grants is refused"
    );

    drop(client);
    daemon.stop().await;
}

/// With `trust_daemon_uid` (the default), the daemon's own uid keeps the
/// shared-operator path on the socket even when a roster exists.
#[cfg(unix)]
#[tokio::test]
async fn the_daemons_own_uid_keeps_the_operator_path_when_trusted() {
    // The kernel peer uid is a property of the local socket.
    let leg = Leg::Local;
    use crate::security::auth_provider::PeercredAuthProvider;
    let tmp = tempfile::tempdir().unwrap();
    let other_uid = PeercredAuthProvider::current_process_uid().wrapping_add(1);
    let mut config = roster_config(tmp.path(), other_uid);
    config.security.trust_daemon_uid = true;
    let daemon = daemon_with(leg, config);
    let mut client = daemon.connect().await;

    let init = client.initialize().await;
    assert_eq!(init.result()["principal_id"], PrincipalId::SHARED_OPERATOR);

    drop(client);
    daemon.stop().await;
}

// ── Endpoint discovery ───────────────────────────────────────────

#[tokio::test]
async fn each_data_dir_gets_its_own_endpoint_and_the_daemon_reports_it() {
    for leg in Leg::ALL {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = daemon_with(leg, config_in(first_dir.path()));
        let second = daemon_with(leg, config_in(second_dir.path()));
        assert_ne!(first.endpoint, second.endpoint);

        for (daemon, dir) in [(&first, first_dir.path()), (&second, second_dir.path())] {
            // A client that knows only the data directory resolves the same
            // endpoint the daemon bound.
            let resolved = socket_path(&Config {
                data_dir: dir.to_path_buf(),
                ..Config::default()
            });
            assert_eq!(resolved, daemon.endpoint);
            #[cfg(unix)]
            assert_eq!(resolved, dir.join("daemon.sock"));
            #[cfg(windows)]
            assert!(
                resolved
                    .to_string_lossy()
                    .starts_with(r"\\.\pipe\zeroclaw-"),
                "{}",
                resolved.display()
            );

            let mut client = daemon.connect().await;
            client.initialize().await.result();
            let status = client.call(Method::Status, json!({})).await;
            assert_eq!(
                status.result()["local_ipc_endpoint"],
                resolved.display().to_string(),
                "{leg}: the daemon reports the endpoint the client resolved"
            );
        }

        first.stop().await;
        second.stop().await;
    }
}

// ── Turn streaming ───────────────────────────────────────────────

/// Updates reach the client while the turn is still running: the provider
/// is held after its first chunk until that chunk has crossed the
/// transport. The turn then ends in exactly one `turn_complete`, after every
/// chunk.
#[tokio::test]
async fn a_turn_streams_its_updates_before_it_completes() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, release) =
            scripted_daemon(leg, tmp.path(), "conformance-streaming", false).await;
        let mut client = daemon.connect().await;
        client.initialize().await.result();

        let (updates, response) =
            run_scripted_turn(&mut client, &release, "conformance-streaming").await;
        // The turn's outcome travels in `turn_complete`; the response is an
        // empty object, kept so request-form callers are answered.
        assert_eq!(*response.result(), json!({}));
        assert_eq!(chunk_texts(&updates), ["alpha ", "beta"]);
        let terminal = updates.last().expect("a terminal event");
        assert_eq!(terminal["type"], "turn_complete");
        assert_eq!(terminal["outcome"], "completed");
        assert_eq!(terminal["content"], "alpha beta");
        assert_eq!(terminal["client_turn_generation"], 9);

        client.barrier().await;
        assert_eq!(
            client.queued_terminals("conformance-streaming"),
            0,
            "{leg}: exactly one terminal event"
        );

        drop(client);
        daemon.stop().await;
    }
}

/// A provider that fails while the turn runs, after a chunk the client has
/// already seen, ends the turn as failed exactly once; the client is not
/// left waiting and its connection keeps serving.
#[tokio::test]
async fn a_provider_failure_mid_turn_ends_it_as_failed_once() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, release) = scripted_daemon(leg, tmp.path(), "conformance-failing", true).await;
        let mut client = daemon.connect().await;
        client.initialize().await.result();

        let (updates, _response) =
            run_scripted_turn(&mut client, &release, "conformance-failing").await;
        assert_eq!(chunk_texts(&updates), ["alpha "]);
        let terminal = updates.last().expect("a terminal event");
        assert_eq!(terminal["outcome"], "failed");
        assert_eq!(terminal["client_turn_generation"], 9);

        client.barrier().await;
        assert_eq!(
            client.queued_terminals("conformance-failing"),
            0,
            "{leg}: exactly one terminal event"
        );

        drop(client);
        daemon.stop().await;
    }
}

#[tokio::test]
async fn a_prompt_on_an_unknown_session_ends_in_a_failed_turn_once() {
    for leg in Leg::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = daemon_with(leg, config_in(tmp.path()));
        let mut client = daemon.connect().await;
        client.initialize().await.result();

        let prompt = client
            .call(
                Method::SessionPrompt,
                json!({
                    "session_id": "conformance-no-such-session",
                    "prompt": "run",
                    "client_turn_generation": 41,
                }),
            )
            .await;
        assert_eq!(prompt.error_code(), i64::from(SESSION_NOT_FOUND));
        let updates = client
            .updates_until_turn_complete("conformance-no-such-session")
            .await;
        let terminal = updates.last().expect("a terminal event");
        assert_eq!(terminal["outcome"], "failed");
        assert_eq!(terminal["client_turn_generation"], 41);

        client.barrier().await;
        assert_eq!(
            client.queued_terminals("conformance-no-such-session"),
            0,
            "{leg}: exactly one terminal event"
        );

        drop(client);
        daemon.stop().await;
    }
}

#[tokio::test]
async fn cancelling_a_running_turn_ends_it_as_cancelled() {
    for leg in Leg::ALL {
        use super::dispatch::connection_test_support::{RUNNING_SID, fixture};
        let tmp = tempfile::tempdir().unwrap();
        let fixture = fixture(tmp.path()).await;
        let daemon = Daemon::start(leg, Arc::clone(&fixture.ctx));
        let mut client = daemon.connect().await;
        client.initialize().await.result();
        // Rebinding the live session makes this connection its owner, which
        // `session/cancel` requires.
        client
            .call(
                Method::SessionNew,
                json!({ "agent_alias": "test-agent", "session_id": RUNNING_SID }),
            )
            .await
            .result();

        let prompt = client
            .send(
                Method::SessionPrompt.wire_name(),
                json!({ "session_id": RUNNING_SID, "prompt": "run" }),
            )
            .await;
        tokio::time::timeout(WAIT, fixture.provider_started.notified())
            .await
            .expect("the turn reaches its provider");

        let cancel = client
            .send(
                Method::SessionCancel.wire_name(),
                json!({ "session_id": RUNNING_SID }),
            )
            .await;
        // Either response may arrive first.
        client.response(prompt).await;
        client.response(cancel).await.result();
        let updates = client.updates_until_turn_complete(RUNNING_SID).await;
        assert_eq!(updates.last().expect("terminal")["outcome"], "cancelled");
        client.barrier().await;
        assert_eq!(
            client.queued_terminals(RUNNING_SID),
            0,
            "{leg}: exactly one terminal event"
        );
        assert!(
            fixture.provider_dropped.load(Ordering::Acquire),
            "{leg}: cancelling the turn drops its provider call"
        );

        drop(client);
        daemon.stop().await;
    }
}

/// Protocol 1 turns belong to the connection that started them: closing it
/// ends the turn. The session itself outlives the connection.
#[tokio::test]
async fn closing_the_prompting_connection_ends_its_turn_but_not_its_session() {
    for leg in Leg::ALL {
        use super::dispatch::connection_test_support::{RUNNING_SID, fixture};
        let tmp = tempfile::tempdir().unwrap();
        let fixture = fixture(tmp.path()).await;
        let daemon = Daemon::start(leg, Arc::clone(&fixture.ctx));
        let mut client = daemon.connect().await;
        client.initialize().await.result();

        client
            .send(
                Method::SessionPrompt.wire_name(),
                json!({ "session_id": RUNNING_SID, "prompt": "run" }),
            )
            .await;
        tokio::time::timeout(WAIT, fixture.provider_started.notified())
            .await
            .expect("the turn reaches its provider");
        assert!(daemon.ctx.sessions.has_inflight_turn(RUNNING_SID));

        drop(client);
        let ended = tokio::time::timeout(WAIT, async {
            while !fixture.provider_dropped.load(Ordering::Acquire)
                || daemon.ctx.sessions.has_inflight_turn(RUNNING_SID)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            ended.is_ok(),
            "{leg}: closing the connection ends the turn it started"
        );
        daemon.wait_for_clients(0).await;

        let mut next = daemon.connect().await;
        next.initialize().await.result();
        let state = next
            .call(Method::SessionState, json!({ "session_id": RUNNING_SID }))
            .await;
        assert_eq!(
            state.result()["state"],
            "idle",
            "{leg}: the session outlives the connection and is idle"
        );

        drop(next);
        daemon.stop().await;
    }
}

// ── Subscriptions ────────────────────────────────────────────────

fn hub_daemon(
    leg: Leg,
    dir: &Path,
    max_frames: usize,
) -> (Daemon, Arc<super::subscription::SubscriptionHub>) {
    use super::subscription::{RingLimits, SubscriptionHub};
    let hub = Arc::new(SubscriptionHub::with_limits(
        RingLimits {
            max_frames,
            max_bytes: usize::MAX,
        },
        usize::MAX,
    ));
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
    let daemon = Daemon::start(
        leg,
        RpcContext::minimal_with_subscription_hub(
            config_in(dir),
            sessions(),
            event_tx,
            Arc::clone(&hub),
        ),
    );
    (daemon, hub)
}

/// A replay cursor that expired before the client subscribed: the lost
/// range is reported, then the buffered frames and live ones follow.
#[tokio::test]
async fn an_expired_replay_cursor_is_told_where_to_resume_then_streams_live() {
    for leg in Leg::ALL {
        use super::subscription::Source;
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, hub) = hub_daemon(leg, tmp.path(), 4);
        for n in 1..=10 {
            hub.publish(Source::Logs, json!({ "n": n }));
        }

        let mut client = daemon.connect().await;
        client.initialize().await.result();
        let subscribed = client
            .call(
                Method::LogsSubscribe,
                json!({ "since_seq": 0, "epoch": hub.epoch() }),
            )
            .await;
        let subscription_id = subscribed.result()["subscription_id"]
            .as_str()
            .expect("subscription id")
            .to_owned();

        let lagged = client.notification().await;
        assert_eq!(lagged["method"], "subscription/lagged", "{lagged}");
        assert_eq!(
            lagged["params"],
            json!({
                "subscription_id": subscription_id,
                "from_seq": 1,
                "resume_seq": 7,
                "epoch_changed": false,
            }),
            "{leg}: the lag notice names the lost range and the resume point"
        );
        let mut replayed = Vec::new();
        for _ in 0..4 {
            let frame = client.notification().await;
            assert_eq!(frame["method"], "logs/event", "{frame}");
            replayed.push(frame["params"]["seq"].as_u64().expect("seq"));
        }
        assert_eq!(replayed, [7, 8, 9, 10]);

        hub.publish(Source::Logs, json!({ "n": 11 }));
        let live = client.notification().await;
        assert_eq!(live["method"], "logs/event");
        assert_eq!(live["params"]["seq"], 11);
        assert_eq!(live["params"]["subscription_id"], subscription_id.as_str());

        drop(client);
        daemon.stop().await;
    }
}

/// A live subscriber that stops reading. Its connection's buffers fill and
/// the subscription's ring evicts behind it; every frame published is
/// either delivered in order or inside a lost range the subscriber is told
/// about, and delivery continues live once it reads again. Another
/// connection keeps being served meanwhile.
#[tokio::test]
async fn a_live_subscriber_that_stops_reading_is_told_what_it_lost() {
    for leg in Leg::ALL {
        use super::subscription::Source;
        const RING: usize = 8;
        const BURST: u64 = 1_500;
        let tmp = tempfile::tempdir().unwrap();
        let (daemon, hub) = hub_daemon(leg, tmp.path(), RING);

        let mut slow = daemon.connect().await;
        slow.initialize().await.result();
        let subscribed = slow.call(Method::LogsSubscribe, json!({})).await;
        let subscription_id = subscribed.result()["subscription_id"]
            .as_str()
            .expect("subscription id")
            .to_owned();

        // A healthy live subscription first.
        let first = hub.publish(Source::Logs, json!({ "n": 0 }));
        let delivered = slow.notification().await;
        assert_eq!(delivered["params"]["seq"], first);

        // Stop reading and publish far more than every buffer between the ring
        // and this client can hold. Publishing is paced so the connection's
        // writer drains into the transport until the transport is full.
        let padding = "x".repeat(4 * 1024);
        for n in 1..=BURST {
            hub.publish(Source::Logs, json!({ "n": n, "padding": padding }));
            if n % RING as u64 == 0 {
                for _ in 0..4 {
                    tokio::task::yield_now().await;
                }
            }
        }
        let last = first + BURST;

        // The stalled subscriber holds up no one else.
        let mut other = daemon.connect().await;
        other.initialize().await.result();
        other.call(Method::Status, json!({})).await.result();

        // Read again: every sequence number after `first` is accounted for.
        let mut expected = first + 1;
        let mut lost_ranges = Vec::new();
        let mut delivered_before_first_loss = 0_u64;
        while expected <= last {
            let frame = slow.notification().await;
            assert_eq!(frame["params"]["subscription_id"], subscription_id.as_str());
            if frame["method"] == "subscription/lagged" {
                let from = frame["params"]["from_seq"].as_u64().expect("from_seq");
                let resume = frame["params"]["resume_seq"].as_u64().expect("resume_seq");
                assert_eq!(
                    from, expected,
                    "{leg}: the lost range starts at the first undelivered frame"
                );
                assert!(resume > from, "{leg}: {frame}");
                assert_eq!(frame["params"]["epoch_changed"], false);
                lost_ranges.push((from, resume));
                expected = resume;
                continue;
            }
            assert_eq!(frame["method"], "logs/event", "{frame}");
            assert_eq!(
                frame["params"]["seq"], expected,
                "{leg}: frames arrive in order, and a gap only after a lag notice"
            );
            if lost_ranges.is_empty() {
                delivered_before_first_loss += 1;
            }
            expected += 1;
        }
        assert!(
            !lost_ranges.is_empty(),
            "{leg}: the burst overflowed the ring behind the stalled reader"
        );
        assert!(
            delivered_before_first_loss > RING as u64,
            "{leg}: frames beyond the ring were buffered by the connection before \
             the loss ({delivered_before_first_loss} delivered)"
        );

        // Delivery continues live.
        let sentinel = hub.publish(Source::Logs, json!({ "n": "sentinel" }));
        let live = slow.notification().await;
        assert_eq!(live["method"], "logs/event");
        assert_eq!(live["params"]["seq"], sentinel);

        drop(slow);
        drop(other);
        daemon.stop().await;
    }
}
