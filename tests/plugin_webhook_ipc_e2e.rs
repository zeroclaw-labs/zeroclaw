//! Two-process proof that the standalone `zeroclaw gateway` forwards
//! channel-plugin webhooks to a running daemon over the local RPC socket:
//! the real daemon owns the route, the dedup store and the guest, and the
//! real gateway process owns HTTP. They run as documented for a gateway next
//! to a daemon: a running daemon owns its config state exclusively, so the
//! gateway has a config dir of its own and reaches the daemon's socket
//! through `ZEROCLAW_SOCKET`.

#![cfg(all(
    unix,
    feature = "agent-runtime",
    feature = "gateway",
    feature = "plugins-wasm-cranelift"
))]

#[path = "support/plugin_channel_fixture.rs"]
mod plugin_channel_fixture;

use std::fs::File;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::{Child, Command};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zeroclaw_api::webhook::PLUGIN_WEBHOOK_DEADLINE;
use zeroclaw_config::multi_agent::{
    AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind, PeerGroupConfig, PeerUsername,
};
use zeroclaw_config::providers::{ChannelRef, ModelProviderRef};
use zeroclaw_config::schema::{Config, CustomModelProviderConfig, ModelProviderConfig};
use zeroclaw_rpc_client::endpoint::SOCKET_ENV;
use zeroclaw_rpc_client::{ConnectOptions, Method, RpcClient};
use zeroclaw_rpc_proto::types::PluginWebhookRoutesResult;

use plugin_channel_fixture::{
    ALIAS, PACKAGE, REPLY, ROUTE, SECRET, SECRET_HEADER, activation_config, install_fixture_package,
};

/// Another alias for the same fixture package: configured instead of
/// [`ALIAS`], it claims the same path as a new owner.
const OTHER_ALIAS: &str = "escalations";
/// A completion request that answers a message made of a marker with this
/// prefix gets its reply held for [`PROVIDER_DELAY`].
const SLOW: &str = "ipc-slow";
const PROVIDER_DELAY: Duration = Duration::from_secs(5);
/// The daemon's log key for a repeated message id it did not deliver again.
const DUPLICATE: &str = "plugin_webhook_duplicate";

const DAEMON_READY: Duration = Duration::from_secs(120);
const GATEWAY_READY: Duration = Duration::from_secs(60);
const RECONNECT: Duration = Duration::from_secs(30);
const RELOAD: Duration = Duration::from_secs(30);
const DELIVERY: Duration = Duration::from_secs(60);
const SETTLE: Duration = Duration::from_secs(3);
const FAIL_FAST: Duration = Duration::from_secs(3);
const FAST_ACK: Duration = Duration::from_secs(2);
const LOGGED: Duration = Duration::from_secs(10);
/// Poll no faster than this, so readiness probes stay far under the webhook
/// rate limit and the gateway's reconnect backoff has room to work.
const POLL: Duration = Duration::from_millis(500);
/// Lines of each child log shown when a test fails.
const LOG_TAIL: usize = 200;

/// One isolated operator install: the daemon's config dir holding the
/// installed fixture package, its config and its data dir; the standalone
/// gateway's own config dir; a scripted model provider; and a separate
/// scratch dir for logs and `HOME`.
struct Instance {
    dir: TempDir,
    gateway_dir: TempDir,
    scratch: TempDir,
    provider: MockServer,
}

impl Instance {
    async fn new() -> Self {
        let dir = install_fixture_package();
        let gateway_dir = TempDir::new().expect("create the gateway's config dir");
        let scratch = TempDir::new().expect("create the scratch dir");
        std::fs::create_dir_all(scratch.path().join("home")).expect("create HOME");
        std::fs::create_dir_all(scratch.path().join("logs")).expect("create the log dir");
        let provider = scripted_provider().await;
        let instance = Self {
            dir,
            gateway_dir,
            scratch,
            provider,
        };
        instance.write_config(Some(ALIAS)).await;
        instance.write_gateway_config().await;
        let socket = instance.socket();
        assert!(
            socket.as_os_str().len() < 100,
            "{} is too long for a Unix socket; point TMPDIR at a shorter directory",
            socket.display()
        );
        instance
    }

    /// Write this install's config: the fixture channel configured under
    /// `alias` and admitting the peer `tester`, or, for `None`, no plugin
    /// channel at all.
    async fn write_config(&self, alias: Option<&str>) {
        let mut config = activation_config(&self.dir, alias.unwrap_or(ALIAS), "5");
        config.providers.models.anthropic.clear();
        config.providers.models.custom.insert(
            "fixture".to_string(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("ipc-test-key".to_string()),
                    uri: Some(format!("{}/v1", self.provider.uri())),
                    model: Some("fixture-model".to_string()),
                    temperature: Some(0.0),
                    ..ModelProviderConfig::default()
                },
            },
        );
        let workspace = self.dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create the agent workspace");
        let agent = config
            .agents
            .get_mut("operator")
            .expect("the activation config declares the operator agent");
        agent.model_provider = ModelProviderRef::new("custom.fixture");
        agent.memory = AgentMemoryConfig {
            backend: MemoryBackendKind::None,
        };
        agent.workspace = AgentWorkspaceConfig {
            path: Some(workspace),
            ..AgentWorkspaceConfig::default()
        };
        // One delivered message makes exactly one model provider request. The
        // reply-intent precheck would add a classifier request that carries
        // the whole conversation as a single user message.
        agent.precheck.enabled = false;
        match alias {
            Some(alias) => {
                config.peer_groups.insert(
                    format!("plugin-{alias}"),
                    PeerGroupConfig {
                        channel: ChannelRef::new(format!("plugin.{alias}")),
                        external_peers: vec![PeerUsername::new("tester")],
                        ..PeerGroupConfig::default()
                    },
                );
            }
            None => {
                agent.channels.clear();
                config.channels.plugin.clear();
                config.plugins.entries.clear();
            }
        }
        config.gateway.host = "127.0.0.1".to_string();
        config.gateway.require_pairing = false;
        config.gateway.webhook_rate_limit_per_minute = 10_000;
        config.memory.backend = "none".to_string();
        config.memory.auto_save = false;
        config.memory.response_cache_enabled = false;
        config.reliability.provider_retries = 0;
        config.reliability.provider_backoff_ms = 0;
        // The default fuel budget traps the fixture's `spin` body within a
        // few seconds, before the ingress deadline this test has to reach.
        config.plugins.limits.call_fuel = 1_000_000_000_000;
        config
            .validate()
            .expect("the IPC fixture is valid operator config");
        assert!(
            config.config_path.starts_with(self.dir.path()),
            "the config must be written inside the instance's temp dir, never over the \
             developer's live config"
        );
        config.save().await.expect("write the isolated config.toml");
    }

    /// Write the standalone gateway's own config, in its own config dir and
    /// with its own data dir. It carries the gateway settings and no plugin
    /// config: the daemon owns the routes.
    async fn write_gateway_config(&self) {
        let mut config = Config {
            data_dir: self.gateway_dir.path().join("data"),
            config_path: self.gateway_dir.path().join("config.toml"),
            ..Config::default()
        };
        config.gateway.host = "127.0.0.1".to_string();
        config.gateway.require_pairing = false;
        config.gateway.webhook_rate_limit_per_minute = 10_000;
        config.memory.backend = "none".to_string();
        config.memory.auto_save = false;
        config.memory.response_cache_enabled = false;
        config
            .validate()
            .expect("the gateway's config is valid operator config");
        assert!(
            config.config_path.starts_with(self.gateway_dir.path()),
            "the gateway's config must be written inside its temp dir"
        );
        config
            .save()
            .await
            .expect("write the gateway's isolated config.toml");
    }

    /// Where the daemon binds for its config: the production default, since
    /// the daemon runs without `ZEROCLAW_SOCKET`. The gateway, whose config
    /// dir is its own, is pointed here through `ZEROCLAW_SOCKET`.
    fn socket(&self) -> PathBuf {
        self.dir.path().join("data").join("daemon.sock")
    }

    fn log(&self, name: &str) -> PathBuf {
        self.scratch.path().join("logs").join(format!("{name}.log"))
    }

    fn read_log(&self, name: &str) -> String {
        std::fs::read_to_string(self.log(name)).unwrap_or_default()
    }

    /// How many lines of the `name` child log mention `needle`.
    fn log_lines(&self, name: &str, needle: &str) -> usize {
        self.read_log(name)
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// Wait until at least `at_least` lines of the `name` child log mention
    /// `needle`, and return how many do.
    async fn wait_for_log_lines(&self, name: &str, needle: &str, at_least: usize) -> usize {
        let deadline = Instant::now() + LOGGED;
        loop {
            let lines = self.log_lines(name, needle);
            if lines >= at_least {
                return lines;
            }
            assert!(
                Instant::now() < deadline,
                "{name} logged {needle} {lines} time(s), never {at_least}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// The tail of every child log.
    fn diagnostics(&self) -> String {
        let mut names: Vec<PathBuf> = std::fs::read_dir(self.scratch.path().join("logs"))
            .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        names.sort();
        let mut out = String::new();
        for path in names {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let tail = &lines[lines.len().saturating_sub(LOG_TAIL)..];
            out.push_str(&format!(
                "\n===== {} (last {} lines) =====\n{}\n",
                path.display(),
                tail.len(),
                tail.join("\n")
            ));
        }
        out
    }

    /// A `zeroclaw` child on `config_dir`: no inherited `ZEROCLAW_*`
    /// override, an isolated `HOME`, and logs on the terminal so they land in
    /// `log`.
    fn zeroclaw(&self, log: &str, config_dir: &std::path::Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("ZEROCLAW_") {
                command.env_remove(&key);
            }
        }
        let log = File::create(self.log(log)).expect("create the child log");
        command
            .arg("--config-dir")
            .arg(config_dir)
            .args(["--verbose", "--log-level", "info"])
            .env_remove("RUST_LOG")
            .env("HOME", self.scratch.path().join("home"))
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone().expect("share the child log"))
            .stderr(log)
            .kill_on_drop(true);
        command
    }

    fn spawn_daemon(&self, n: usize) -> Child {
        self.zeroclaw(&format!("daemon-{n}"), self.dir.path())
            .args(["daemon", "--host", "127.0.0.1", "--port", "0"])
            .spawn()
            .expect("spawn the daemon")
    }

    /// The standalone gateway on its own config dir, pointed at the daemon's
    /// socket.
    fn spawn_gateway(&self, n: usize, port: u16) -> Child {
        self.zeroclaw(&format!("gateway-{n}"), self.gateway_dir.path())
            .env(SOCKET_ENV, self.socket())
            .args(["gateway", "start", "--host", "127.0.0.1", "-p"])
            .arg(port.to_string())
            .spawn()
            .expect("spawn the standalone gateway")
    }

    /// Wait until the daemon has published the fixture route with its owner.
    async fn wait_for_route(&self, daemon: &mut Child) {
        self.wait_for_owner(daemon, Some(ALIAS)).await;
    }

    /// Wait until the daemon's route listing shows [`ROUTE`] owned by the
    /// fixture package under `alias`, or, for `None`, shows no owner for it.
    async fn wait_for_owner(&self, daemon: &mut Child, alias: Option<&str>) {
        let deadline = Instant::now() + DAEMON_READY;
        loop {
            if let Some(status) = daemon.try_wait().expect("poll the daemon") {
                panic!("the daemon exited with {status} while /plugin/{ROUTE} awaited {alias:?}");
            }
            if let Some(listed) = self.routes().await {
                let owner = listed
                    .routes
                    .iter()
                    .find(|route| route.path == ROUTE)
                    .map(|route| (route.plugin.as_str(), route.channel_alias.as_str()));
                if owner == alias.map(|alias| (PACKAGE, alias)) {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the daemon never listed /plugin/{ROUTE} with owner alias {alias:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// Reload the daemon over RPC and wait until the reload has retired the
    /// generation that took the request.
    async fn reload(&self) {
        let admin = self
            .connect()
            .await
            .expect("the test connects to the daemon");
        admin
            .request(Method::ConfigReload, json!({}))
            .await
            .expect("the daemon schedules a reload");
        tokio::time::timeout(RELOAD, admin.closed())
            .await
            .expect("the reload retires the connection's generation");
    }

    /// The daemon's routes over a fresh admin connection, or `None` while
    /// its endpoint does not answer.
    async fn routes(&self) -> Option<PluginWebhookRoutesResult> {
        let client = self.connect().await?;
        let routes = client
            .call::<PluginWebhookRoutesResult>(Method::PluginWebhookRoutes, json!({}))
            .await
            .ok();
        client.shutdown();
        routes
    }

    async fn connect(&self) -> Option<RpcClient> {
        RpcClient::connect_local(
            &self.socket(),
            ConnectOptions {
                handshake_timeout: Some(Duration::from_secs(2)),
                ..ConnectOptions::default()
            },
        )
        .await
        .ok()
    }

    /// How many model provider requests answered a message carrying
    /// `marker`. With the reply-intent precheck off, each delivered message
    /// makes exactly one.
    async fn deliveries(&self, marker: &str) -> usize {
        self.provider
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                last_user_text(&body).contains(marker)
            })
            .count()
    }

    async fn wait_for_deliveries(&self, marker: &str, at_least: usize) {
        let deadline = Instant::now() + DELIVERY;
        while self.deliveries(marker).await < at_least {
            assert!(
                Instant::now() < deadline,
                "{marker} never reached the model provider {at_least} time(s)"
            );
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.diagnostics());
        }
    }
}

// ── Scripted model provider ─────────────────────────────────────────────

async fn scripted_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_completion)
        .mount(&server)
        .await;
    server
}

/// An OpenAI-compatible completion of [`REPLY`], streamed when the request
/// streams.
fn chat_completion(request: &Request) -> ResponseTemplate {
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    let delay = if answers_slow_message(&body) {
        PROVIDER_DELAY
    } else {
        Duration::ZERO
    };
    let usage = json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15});
    let template = if body["stream"].as_bool() == Some(true) {
        let first = json!({"id": "chatcmpl-ipc", "model": "fixture-model",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": REPLY}}]});
        let last = json!({"id": "chatcmpl-ipc", "model": "fixture-model",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": usage});
        ResponseTemplate::new(200).set_body_raw(
            format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n"),
            "text/event-stream",
        )
    } else {
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-ipc",
            "object": "chat.completion",
            "model": "fixture-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": REPLY},
                "finish_reason": "stop"
            }],
            "usage": usage,
        }))
    };
    template.set_delay(delay)
}

/// The text of the last user message in a completion request. Only the last
/// one counts, so earlier turns of the same conversation are not recounted.
fn last_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
        .map(|message| match &message["content"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// Whether a completion request answers a [`SLOW`] message: the last word of
/// its last user message, where a channel turn puts the inbound content after
/// its context preamble, is a slow marker. A slow message that is only
/// earlier in the conversation does not hold back later replies.
fn answers_slow_message(body: &Value) -> bool {
    last_user_text(body)
        .split_whitespace()
        .next_back()
        .is_some_and(|word| word.starts_with(SLOW))
}

// ── HTTP ────────────────────────────────────────────────────────────────

struct Answer {
    status: u16,
    content_type: Option<String>,
    body: String,
    elapsed: Duration,
}

async fn http(
    port: u16,
    method: reqwest::Method,
    path_and_query: &str,
    secret: Option<&str>,
    body: Option<String>,
) -> Answer {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .build()
        .expect("build the HTTP client");
    let mut request = client.request(method, format!("http://127.0.0.1:{port}{path_and_query}"));
    if let Some(secret) = secret {
        request = request.header(SECRET_HEADER, secret);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    let started = Instant::now();
    let response = request
        .send()
        .await
        .unwrap_or_else(|error| panic!("{path_and_query}: the gateway did not answer: {error}"));
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let body = response.text().await.expect("read the response body");
    Answer {
        status,
        content_type,
        body,
        elapsed: started.elapsed(),
    }
}

async fn post(port: u16, body: impl Into<String>, secret: &str) -> Answer {
    http(
        port,
        reqwest::Method::POST,
        &format!("/plugin/{ROUTE}"),
        Some(secret),
        Some(body.into()),
    )
    .await
}

async fn get(port: u16, query: &str) -> Answer {
    http(
        port,
        reqwest::Method::GET,
        &format!("/plugin/{ROUTE}?{query}"),
        Some(SECRET),
        None,
    )
    .await
}

fn message(id: &str, content: &str) -> String {
    json!({"id": id, "sender": "tester", "reply_target": "room", "content": content}).to_string()
}

/// A marker no earlier run or step has used.
fn marker(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!("{prefix}-{nanos}")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("probe a free port")
        .local_addr()
        .expect("read the probed port")
        .port()
}

async fn wait_for_health(port: u16, gateway: &mut Child) {
    let deadline = Instant::now() + GATEWAY_READY;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .expect("build the HTTP client");
    loop {
        if let Some(status) = gateway.try_wait().expect("poll the gateway") {
            panic!("the standalone gateway exited with {status} before it served /health");
        }
        let healthy = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        if healthy {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the standalone gateway never served /health"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Post `body` until the gateway answers anything but the `503` it gives while
/// its connection to the daemon is being re-established, and return that
/// answer.
async fn post_once_connected(port: u16, body: &str, within: Duration) -> Answer {
    let deadline = Instant::now() + within;
    loop {
        let answer = post(port, body, SECRET).await;
        if answer.status != 503 {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "the gateway never reconnected to the daemon"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until a signed challenge probe round-trips through the gateway, the
/// daemon and the guest.
async fn wait_for_forwarding(port: u16, within: Duration) {
    let deadline = Instant::now() + within;
    loop {
        let answer = get(port, "probe=ready").await;
        if answer.status == 200 && answer.body == "probe=ready" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the gateway never forwarded a probe; last answer {} {:?}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL).await;
    }
}

async fn stop(child: &mut Child) {
    child.start_kill().expect("signal the child");
    child.wait().await.expect("reap the child");
}

// ── Tests ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_gateway_forwards_plugin_webhooks_through_the_daemon() {
    let instance = Instance::new().await;
    let mut daemon = instance.spawn_daemon(1);
    instance.wait_for_route(&mut daemon).await;
    let port = free_port();
    let mut gateway = instance.spawn_gateway(1, port);
    wait_for_health(port, &mut gateway).await;
    wait_for_forwarding(port, RECONNECT).await;

    // A GET challenge echoes its query as the reply.
    let answer = get(port, "challenge=ipc-echo").await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (200, "challenge=ipc-echo")
    );
    assert!(
        answer
            .content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("text/plain")),
        "{:?}",
        answer.content_type
    );

    // A POST challenge replies without reaching the agent.
    let answer = post(
        port,
        json!({"challenge": "ipc-challenge"}).to_string(),
        SECRET,
    )
    .await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (200, "ipc-challenge")
    );

    // A wrong credential is refused by the guest.
    let denied = marker("ipc-denied");
    let answer = post(port, message("ipc-denied-1", &denied), "wrong").await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (401, "unauthorized webhook")
    );

    // A path nobody owns is resolved by the daemon, not the gateway.
    let answer = http(
        port,
        reqwest::Method::GET,
        "/plugin/unclaimed",
        Some(SECRET),
        None,
    )
    .await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (404, "webhook not found")
    );

    // The webhook is acknowledged once the message is queued for the agent,
    // well before the agent's turn, whose model reply is held back.
    let slow = marker(SLOW);
    let answer = post(port, message("ipc-slow-1", &slow), SECRET).await;
    assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    assert!(
        answer.elapsed < FAST_ACK,
        "the ack waited {:?} for work that belongs after it",
        answer.elapsed
    );
    instance.wait_for_deliveries(&slow, 1).await;

    // One message id is delivered once, however often it is posted: the
    // daemon acknowledges the repeat and logs it as a duplicate.
    let duplicate = marker("ipc-dup");
    for _ in 0..2 {
        let answer = post(port, message("ipc-dup-1", &duplicate), SECRET).await;
        assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    }
    instance.wait_for_deliveries(&duplicate, 1).await;
    let duplicates = instance.wait_for_log_lines("daemon-1", DUPLICATE, 1).await;
    assert_eq!(duplicates, 1);

    // Dedup lives in the daemon, so a new gateway process keeps it. The first
    // delivery has reached the model before the gateway stops, so the repeat
    // after the restart is counted against a finished delivery, and the
    // daemon, not only the absence of a second delivery, shows it was
    // recognized.
    stop(&mut gateway).await;
    let mut gateway = instance.spawn_gateway(2, port);
    wait_for_health(port, &mut gateway).await;
    wait_for_forwarding(port, RECONNECT).await;
    let answer = post(port, message("ipc-dup-1", &duplicate), SECRET).await;
    assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    instance
        .wait_for_log_lines("daemon-1", DUPLICATE, duplicates + 1)
        .await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(instance.deliveries(&duplicate).await, 1);

    assert_eq!(instance.deliveries(&slow).await, 1);
    assert_eq!(instance.deliveries("ipc-challenge").await, 0);
    assert_eq!(instance.deliveries(&denied).await, 0);
    assert!(daemon.try_wait().expect("poll the daemon").is_none());
    assert!(gateway.try_wait().expect("poll the gateway").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_gateway_fails_fast_without_the_daemon_and_recovers_after_restart() {
    let instance = Instance::new().await;
    let port = free_port();
    let mut gateway = instance.spawn_gateway(1, port);
    wait_for_health(port, &mut gateway).await;

    // No daemon yet: every path fails fast instead of queueing.
    let early = marker("ipc-early");
    let answer = post(port, message("ipc-early-1", &early), SECRET).await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (503, "webhook unavailable")
    );
    assert!(answer.elapsed < FAIL_FAST, "{:?}", answer.elapsed);

    // A daemon that starts after the gateway is found without a restart.
    let mut daemon = instance.spawn_daemon(1);
    instance.wait_for_route(&mut daemon).await;
    wait_for_forwarding(port, RECONNECT).await;

    // The core's own deadline ends a guest that never answers, and the next
    // request is served by a fresh guest instance. The gateway not logging a
    // timeout of its own is what shows the core, not the gateway, ended the
    // request; the upper bound only catches a hang, with room for a loaded
    // runner.
    let answer = post(port, "spin", SECRET).await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (504, "webhook processing timed out")
    );
    assert!(
        answer.elapsed >= PLUGIN_WEBHOOK_DEADLINE
            && answer.elapsed < PLUGIN_WEBHOOK_DEADLINE + Duration::from_secs(5),
        "the request ended {:?} after it was sent",
        answer.elapsed
    );
    assert!(
        !instance
            .read_log("gateway-1")
            .contains("plugin_webhook_core_timeout"),
        "the core answered `timeout` itself"
    );
    let answer = get(port, "challenge=ipc-recovered").await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (200, "challenge=ipc-recovered")
    );

    // A daemon killed mid-request fails the request at once, not at the
    // deadline.
    let pending = zeroclaw_spawn::spawn!(async move { post(port, "spin", SECRET).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    stop(&mut daemon).await;
    let killed_at = Instant::now();
    let answer = pending.await.expect("the pending request joins");
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (503, "webhook unavailable")
    );
    assert!(killed_at.elapsed() < FAIL_FAST, "{:?}", killed_at.elapsed());

    let down = marker("ipc-down");
    let answer = post(port, message("ipc-down-1", &down), SECRET).await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (503, "webhook unavailable")
    );
    assert!(answer.elapsed < FAIL_FAST, "{:?}", answer.elapsed);

    // A restarted daemon on the same config is reconnected to.
    let mut daemon = instance.spawn_daemon(2);
    instance.wait_for_route(&mut daemon).await;
    wait_for_forwarding(port, RECONNECT).await;
    let after = marker("ipc-after");
    let answer = post(port, message("ipc-after-1", &after), SECRET).await;
    assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    instance.wait_for_deliveries(&after, 1).await;

    // A daemon reload closes every local connection with its generation; the
    // gateway reconnects to the next one.
    instance.reload().await;
    instance.wait_for_route(&mut daemon).await;
    wait_for_forwarding(port, RECONNECT).await;
    let reloaded = marker("ipc-reloaded");
    let answer = post(port, message("ipc-reloaded-1", &reloaded), SECRET).await;
    assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    instance.wait_for_deliveries(&reloaded, 1).await;

    // Each accepted message reached the model once, and nothing refused while
    // the daemon was away reached it later.
    tokio::time::sleep(SETTLE).await;
    assert_eq!(instance.deliveries(&after).await, 1);
    assert_eq!(instance.deliveries(&reloaded).await, 1);
    assert_eq!(instance.deliveries(&early).await, 0);
    assert_eq!(instance.deliveries(&down).await, 0);

    assert!(daemon.try_wait().expect("poll the daemon").is_none());
    assert!(
        gateway.try_wait().expect("poll the gateway").is_none(),
        "one gateway process served the whole test"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reloaded_daemon_decides_who_owns_a_forwarded_path() {
    let instance = Instance::new().await;
    let mut daemon = instance.spawn_daemon(1);
    instance.wait_for_route(&mut daemon).await;
    let port = free_port();
    let mut gateway = instance.spawn_gateway(1, port);
    wait_for_health(port, &mut gateway).await;
    wait_for_forwarding(port, RECONNECT).await;

    // The first owner delivers a message id once, however often it is posted.
    let moved = marker("ipc-moved");
    for _ in 0..2 {
        let answer = post(port, message("ipc-moved-1", &moved), SECRET).await;
        assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    }
    instance.wait_for_deliveries(&moved, 1).await;
    instance.wait_for_log_lines("daemon-1", DUPLICATE, 1).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(instance.deliveries(&moved).await, 1);

    // With the channel removed, the reloaded daemon lists no owner for the
    // path and answers the forwarded request itself: the gateway holds no
    // route table, and says 503 rather than 404 while it cannot ask.
    instance.write_config(None).await;
    instance.reload().await;
    instance.wait_for_owner(&mut daemon, None).await;
    let orphan = marker("ipc-orphan");
    let answer = post_once_connected(port, &message("ipc-orphan-1", &orphan), RECONNECT).await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (404, "webhook not found")
    );

    // The same package under another alias claims the path as a new owner,
    // and the message id the old owner delivered is delivered again, once.
    // A reload rebuilds the ingress and its dedup store, so this step proves
    // that the reloaded daemon routes to the new owner and dedups in a fresh
    // namespace; that dedup is keyed by owner within one generation is proven
    // by the infra unit test
    // `dedup_is_keyed_by_route_owner_and_survives_republication`.
    instance.write_config(Some(OTHER_ALIAS)).await;
    instance.reload().await;
    instance
        .wait_for_owner(&mut daemon, Some(OTHER_ALIAS))
        .await;
    wait_for_forwarding(port, RECONNECT).await;
    for _ in 0..2 {
        let answer = post(port, message("ipc-moved-1", &moved), SECRET).await;
        assert_eq!((answer.status, answer.body.as_str()), (200, ""));
    }
    instance.wait_for_deliveries(&moved, 2).await;
    instance.wait_for_log_lines("daemon-1", DUPLICATE, 2).await;
    tokio::time::sleep(SETTLE).await;
    assert_eq!(instance.deliveries(&moved).await, 2);
    assert_eq!(instance.deliveries(&orphan).await, 0);

    assert!(daemon.try_wait().expect("poll the daemon").is_none());
    assert!(
        gateway.try_wait().expect("poll the gateway").is_none(),
        "one gateway process served the whole test"
    );
}
