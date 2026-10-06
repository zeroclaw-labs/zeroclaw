//! Golden-frame recorder and replayer for the gateway's wire surfaces.
//!
//! Each scenario starts the real gateway (`run_gateway`, every route mounted)
//! on a loopback port, points its agent at a scripted OpenAI-compatible
//! provider, drives one client conversation over WebSocket chat, webhook JSON,
//! webhook SSE, the `/api/events` stream, or ACP, and records what the client
//! sent and received. Fields that differ between runs are replaced with stable
//! placeholders, and the result is compared with the checked-in fixture under
//! `tests/golden/`.
//!
//! The scenarios only talk to the gateway over the network, so the same
//! transcripts can be replayed after routes move behind the core RPC boundary
//! and against a separately built gateway binary.
//!
//! These tests are `#[ignore]`d: the required test job skips them, and an
//! informational CI step runs them with `--run-ignored only`. To regenerate
//! the fixtures after an intended change:
//!
//! ```text
//! ZEROCLAW_GOLDEN_RECORD=1 cargo nextest run -p zeroclaw-gateway \
//!     --test golden_frames --run-ignored only
//! ```
//!
//! The `/plugin/{path}` scenarios in the `plugin_webhook` module are the
//! exception. The route only exists with the gateway's `plugins-wasm` feature,
//! so the module is compiled only with it too, and its scenarios are not
//! ignored: the plugin backend CI job runs them as required coverage. They
//! start the gateway through `run_gateway_with_plugin_webhooks` with a
//! test-owned plugin webhook ingress whose routes are served by scripted
//! workers in place of channel plugins, and they also record what each worker
//! received.
//! To regenerate their fixtures:
//!
//! ```text
//! ZEROCLAW_GOLDEN_RECORD=1 cargo test -p zeroclaw-gateway \
//!     --features plugins-wasm --test golden_frames plugin_webhook
//! ```
//!
//! Normalization keeps identity visible: each distinct UUID becomes
//! `<uuid:N>` numbered by first appearance, so a transcript still shows
//! whether two frames name the same session, turn, or tool call.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

use zeroclaw_config::multi_agent::{AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};

const RECORD_ENV: &str = "ZEROCLAW_GOLDEN_RECORD";
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
const STREAM_IDLE: Duration = Duration::from_secs(3);
const AGENT: &str = "web";

// ── Scripted model provider ─────────────────────────────────────────────

/// One scripted provider reply, consumed in order; the last one repeats.
#[derive(Clone)]
enum Reply {
    /// Assistant text, streamed word by word when the request streams.
    Text(&'static str),
    /// An HTTP error status with a JSON error body.
    Status(u16),
}

struct ScriptedProvider {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl ScriptedProvider {
    async fn spawn(script: Vec<Reply>) -> Self {
        let script = Arc::new(script);
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |axum::Json(request): axum::Json<Value>| {
                let script = Arc::clone(&script);
                let counter = Arc::clone(&counter);
                async move {
                    let index = counter.fetch_add(1, Ordering::SeqCst);
                    let reply = script
                        .get(index)
                        .or_else(|| script.last())
                        .cloned()
                        .unwrap_or(Reply::Text(""));
                    let stream = request.get("stream").and_then(Value::as_bool) == Some(true);
                    provider_response(&reply, stream)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted provider");
        let addr = listener.local_addr().expect("scripted provider address");
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("serve scripted provider");
        });
        Self {
            addr,
            requests,
            server,
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn provider_response(reply: &Reply, stream: bool) -> Response {
    let text = match reply {
        Reply::Status(code) => {
            let status = StatusCode::from_u16(*code).expect("scripted status code");
            let body =
                json!({"error": {"message": "scripted provider failure", "type": "server_error"}});
            return (status, axum::Json(body)).into_response();
        }
        Reply::Text(text) => *text,
    };
    let usage = json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15});
    if !stream {
        let body = json!({
            "id": "chatcmpl-golden",
            "object": "chat.completion",
            "model": "fixture-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"
            }],
            "usage": usage,
        });
        return axum::Json(body).into_response();
    }
    let mut sse = String::new();
    for (i, word) in text.split_inclusive(' ').enumerate() {
        let delta = if i == 0 {
            json!({"role": "assistant", "content": word})
        } else {
            json!({"content": word})
        };
        let chunk = json!({"id": "chatcmpl-golden", "model": "fixture-model",
            "choices": [{"index": 0, "delta": delta}]});
        sse.push_str(&format!("data: {chunk}\n\n"));
    }
    let last = json!({"id": "chatcmpl-golden", "model": "fixture-model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": usage});
    sse.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from(sse),
    )
        .into_response()
}

// ── Gateway under test ──────────────────────────────────────────────────

struct Gateway {
    addr: SocketAddr,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    root: tempfile::TempDir,
}

impl Gateway {
    async fn start(provider: &ScriptedProvider) -> Self {
        let root = tempfile::TempDir::new().expect("gateway temp root");
        let config = fixture_config(root.path(), &provider.base_url());
        Self::launch(root, move |port, reload_controls, readiness| {
            zeroclaw_gateway::run_gateway(
                "127.0.0.1",
                port,
                config,
                None,
                Some(reload_controls),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(readiness),
            )
        })
        .await
    }

    /// Spawns the gateway that `run` starts on a free loopback port and waits
    /// until it reports its bind.
    async fn launch<F, Fut>(root: tempfile::TempDir, run: F) -> Self
    where
        F: FnOnce(
            u16,
            zeroclaw_runtime::daemon::GatewayReloadControls,
            zeroclaw_runtime::daemon::GatewayReadinessReporter,
        ) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe free port");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);

        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (reload_tx, _) = tokio::sync::watch::channel(false);
        let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls::standalone(
            shutdown_tx.clone(),
            reload_tx,
        );
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(None);
        let readiness = zeroclaw_runtime::daemon::GatewayReadinessReporter::new(move |addr| {
            let _ = ready_tx.send(Some(addr));
        });
        let gateway = run(port, reload_controls, readiness);
        let server = zeroclaw_spawn::spawn!(gateway);
        let addr = tokio::time::timeout(STEP_TIMEOUT, async {
            ready_rx
                .wait_for(Option::is_some)
                .await
                .expect("gateway readiness channel");
            ready_rx.borrow().expect("gateway bound address")
        })
        .await
        .expect("gateway should report its bind");
        Self {
            addr,
            shutdown_tx,
            server,
            root,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.server).await;
    }
}

fn fixture_config(root: &Path, provider_url: &str) -> Config {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("fixture workspace");
    let mut config = Config {
        data_dir: workspace.clone(),
        config_path: root.join("config.toml"),
        ..Config::default()
    };
    config.gateway.require_pairing = false;
    config.memory.backend = "none".to_string();
    config.memory.auto_save = false;
    config.memory.response_cache_enabled = false;
    config.reliability.provider_retries = 0;
    config.reliability.provider_backoff_ms = 0;
    config.providers.models.custom.insert(
        "fixture".to_string(),
        CustomModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("golden-test-key".to_string()),
                uri: Some(provider_url.to_string()),
                model: Some("fixture-model".to_string()),
                temperature: Some(0.0),
                ..ModelProviderConfig::default()
            },
        },
    );
    config.risk_profiles.insert(
        "fixture".to_string(),
        RiskProfileConfig {
            allowed_tools: vec!["__golden_fixture_no_tools__".to_string()],
            ..RiskProfileConfig::default()
        },
    );
    config.runtime_profiles.insert(
        "fixture".to_string(),
        RuntimeProfileConfig {
            max_tool_iterations: 1,
            ..RuntimeProfileConfig::default()
        },
    );
    config.agents.insert(
        AGENT.to_string(),
        AliasedAgentConfig {
            model_provider: "custom.fixture".into(),
            risk_profile: "fixture".into(),
            runtime_profile: "fixture".into(),
            memory: AgentMemoryConfig {
                backend: MemoryBackendKind::None,
            },
            workspace: AgentWorkspaceConfig {
                path: Some(workspace),
                ..AgentWorkspaceConfig::default()
            },
            ..AliasedAgentConfig::default()
        },
    );
    config
}

// ── Transcript and normalization ────────────────────────────────────────

#[derive(Default)]
struct Transcript {
    frames: Vec<Value>,
}

impl Transcript {
    fn send(&mut self, channel: &str, payload: Value) {
        self.frames
            .push(json!({"dir": "send", "channel": channel, "payload": payload}));
    }

    fn recv(&mut self, channel: &str, payload: Value) {
        self.frames
            .push(json!({"dir": "recv", "channel": channel, "payload": payload}));
    }
}

/// Replaces values that change between runs with stable placeholders.
struct Normalizer {
    replacements: Vec<(String, String)>,
    uuids: HashMap<String, usize>,
}

impl Normalizer {
    fn new(gateway: &Gateway, provider: &ScriptedProvider) -> Self {
        let mut replacements = Vec::new();
        // Canonicalized and raw spellings of the temp root (macOS /private).
        let root = gateway.root.path();
        if let Ok(canonical) = root.canonicalize() {
            replacements.push((canonical.to_string_lossy().into_owned(), "<root>".into()));
        }
        replacements.push((root.to_string_lossy().into_owned(), "<root>".into()));
        replacements.push((gateway.addr.to_string(), "<gateway>".into()));
        replacements.push((
            format!("localhost:{}", gateway.addr.port()),
            "<gateway>".into(),
        ));
        replacements.push((provider.addr.to_string(), "<provider>".into()));
        replacements.push((env!("CARGO_PKG_VERSION").to_string(), "<version>".into()));
        Self::with_replacements(replacements)
    }

    fn with_replacements(replacements: Vec<(String, String)>) -> Self {
        Self {
            replacements,
            uuids: HashMap::new(),
        }
    }

    fn value(&mut self, key: Option<&str>, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.value(Some(k), v)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.value(key, v)).collect()),
            Value::String(s) => Value::String(self.string(s)),
            Value::Number(_) if key.is_some_and(is_timing_key) => {
                Value::String("<duration>".into())
            }
            Value::Number(_) if key.is_some_and(is_clock_key) => {
                Value::String("<timestamp>".into())
            }
            other => other.clone(),
        }
    }

    fn string(&mut self, s: &str) -> String {
        if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
            return "<timestamp>".into();
        }
        let mut out = s.to_string();
        for (from, to) in &self.replacements {
            if !from.is_empty() {
                out = out.replace(from.as_str(), to);
            }
        }
        self.replace_uuids(&out)
    }

    fn replace_uuids(&mut self, s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < bytes.len() {
            // Match on bytes: slicing the `str` 36 bytes ahead could land
            // inside a multibyte character. A match is all ASCII, so both of
            // its ends are character boundaries.
            if let Some(candidate) = bytes.get(i..i + 36)
                && is_uuid(candidate)
            {
                let found = s[i..i + 36].to_ascii_lowercase();
                let next = self.uuids.len() + 1;
                let n = *self.uuids.entry(found).or_insert(next);
                out.push_str(&format!("<uuid:{n}>"));
                i += 36;
            } else {
                let ch = s[i..].chars().next().expect("char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        out
    }
}

fn is_uuid(candidate: &[u8]) -> bool {
    candidate.len() == 36
        && candidate.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn is_timing_key(key: &str) -> bool {
    key.ends_with("_ms") || key == "duration" || key == "elapsed"
}

fn is_clock_key(key: &str) -> bool {
    matches!(
        key,
        "timestamp" | "ts" | "created" | "created_at" | "updated_at"
    )
}

/// Merges consecutive WebSocket `chunk` frames. How the gateway batches
/// streamed text depends on scheduling, so only the joined text is stable.
fn coalesce_chunks(frames: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for frame in frames {
        let is_chunk = |f: &Value| {
            f["dir"] == "recv" && f["channel"] == "ws" && f["payload"]["type"] == "chunk"
        };
        if is_chunk(&frame)
            && let Some(prev) = out.last_mut()
            && is_chunk(prev)
        {
            let joined = format!(
                "{}{}",
                prev["payload"]["content"].as_str().unwrap_or_default(),
                frame["payload"]["content"].as_str().unwrap_or_default()
            );
            prev["payload"]["content"] = Value::String(joined);
            continue;
        }
        out.push(frame);
    }
    out
}

// ── Fixture comparison ──────────────────────────────────────────────────

fn fixture_path(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{scenario}.json"))
}

fn check_fixture(scenario: &str, frames: Vec<Value>) {
    let document = json!({"scenario": scenario, "frames": frames});
    let mut rendered = serde_json::to_string_pretty(&document).expect("render transcript");
    rendered.push('\n');
    let path = fixture_path(scenario);
    if std::env::var_os(RECORD_ENV).is_some() {
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
        std::fs::write(&path, &rendered).expect("write fixture");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden fixture {}; record it with {RECORD_ENV}=1",
            path.display()
        )
    });
    if expected != rendered {
        panic!(
            "golden-frame mismatch for `{scenario}` ({}):\n{}\nIf the change is intended, re-record with {RECORD_ENV}=1 and review the fixture diff.",
            path.display(),
            line_diff(&expected, &rendered)
        );
    }
}

/// Minimal line diff (LCS) for readable mismatch reports.
fn line_diff(expected: &str, actual: &str) -> String {
    let a: Vec<&str> = expected.lines().collect();
    let b: Vec<&str> = actual.lines().collect();
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = String::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push_str(&format!("+{:>4} {}\n", j + 1, b[j]));
            j += 1;
        } else {
            out.push_str(&format!("-{:>4} {}\n", i + 1, a[i]));
            i += 1;
        }
    }
    out
}

/// Serializes scenarios within one test process. Observer events reach
/// `/api/events` through a process-wide hook, so a gateway started by a
/// concurrent scenario would add its turns to another scenario's stream.
/// nextest already runs each test in its own process.
static SCENARIO_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs a standalone-gateway scenario against its scripted provider.
fn run_scenario<F, Fut>(scenario: &'static str, script: Vec<Reply>, body: F)
where
    F: FnOnce(SocketAddr, Transcript) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Transcript>,
{
    on_scenario_runtime(scenario, move || async move {
        let provider = ScriptedProvider::spawn(script).await;
        let gateway = Gateway::start(&provider).await;
        let transcript = tokio::time::timeout(
            Duration::from_secs(60),
            body(gateway.addr, Transcript::default()),
        )
        .await
        .unwrap_or_else(|_| panic!("scenario `{scenario}` timed out"));
        let mut normalizer = Normalizer::new(&gateway, &provider);
        let frames = coalesce_chunks(transcript.frames)
            .iter()
            .map(|frame| normalizer.value(None, frame))
            .collect();
        let provider_requests = provider.requests.load(Ordering::SeqCst);
        gateway.stop().await;
        let mut frames: Vec<Value> = frames;
        frames.push(json!({"dir": "meta", "channel": "provider",
            "payload": {"requests": provider_requests}}));
        check_fixture(scenario, frames);
    });
}

/// Runs one scenario at a time on a runtime with large worker stacks:
/// building a runtime agent overflows the default test-thread stack on Linux.
fn on_scenario_runtime<F, Fut>(scenario: &'static str, run: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let _serial = SCENARIO_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::thread::Builder::new()
        .name(format!("golden-{scenario}"))
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("scenario runtime");
            runtime.block_on(run());
        })
        .expect("spawn scenario thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

// ── Minimal HTTP/1.1 client ─────────────────────────────────────────────

struct HttpResponse {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> HttpResponse {
    let raw = http_exchange(addr, method, path, headers, body, None).await;
    parse_http_response(&raw)
}

/// Sends one request with `Connection: close` and reads until EOF, or until
/// `stop_after` appears in the decoded body (for endless streams).
async fn http_exchange(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
    stop_after: Option<&str>,
) -> Vec<u8> {
    let mut stream = open_request(addr, method, path, headers, body).await;
    read_response(&mut stream, Vec::new(), stop_after).await
}

async fn open_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> tokio::net::TcpStream {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut raw_headers: Vec<(&str, &[u8])> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_bytes()))
        .collect();
    if body.is_some() {
        raw_headers.push(("Content-Type", "application/json".as_bytes()));
    }
    let mut request = request_head(addr, method, path, &raw_headers, payload.len());
    request.extend_from_slice(payload.as_bytes());
    write_request(addr, &request).await
}

/// The request line and headers of a `Connection: close` request whose body
/// is `content_length` bytes. Header values are raw bytes, so a request can
/// carry values that are not UTF-8.
fn request_head(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &[u8])],
    content_length: usize,
) -> Vec<u8> {
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n").into_bytes();
    for (name, value) in headers {
        head.extend_from_slice(name.as_bytes());
        head.extend_from_slice(b": ");
        head.extend_from_slice(value);
        head.extend_from_slice(b"\r\n");
    }
    head.extend_from_slice(format!("Content-Length: {content_length}\r\n\r\n").as_bytes());
    head
}

async fn write_request(addr: SocketAddr, request: &[u8]) -> tokio::net::TcpStream {
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to gateway");
    stream.write_all(request).await.expect("write request");
    stream
}

/// Reads until the response head is complete; returns the bytes read.
async fn read_response_head(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
        let read = tokio::time::timeout(STEP_TIMEOUT, stream.read(&mut buf))
            .await
            .expect("response head timed out")
            .expect("read response head");
        assert!(read > 0, "connection closed before the response head");
        raw.extend_from_slice(&buf[..read]);
    }
    raw
}

async fn read_response(
    stream: &mut tokio::net::TcpStream,
    mut raw: Vec<u8>,
    stop_after: Option<&str>,
) -> Vec<u8> {
    let mut buf = [0u8; 8192];
    loop {
        if let Some(needle) = stop_after
            && parse_http_response(&raw).body.contains(needle)
        {
            break;
        }
        // An endless stream ends the read once it has been quiet for a while.
        let idle = if stop_after.is_some() {
            STREAM_IDLE
        } else {
            STEP_TIMEOUT
        };
        let read = match tokio::time::timeout(idle, stream.read(&mut buf)).await {
            Ok(read) => read.expect("read response"),
            Err(_) if stop_after.is_some() => break,
            Err(_) => panic!("gateway response timed out"),
        };
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..read]);
    }
    raw
}

fn parse_http_response(raw: &[u8]) -> HttpResponse {
    // Split and de-chunk on raw bytes, and decode only the reassembled body:
    // chunk sizes count bytes, and a multibyte character may straddle two
    // chunks, so decoding before de-chunking would misalign or corrupt it.
    let (head, rest) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(end) => (&raw[..end], &raw[end + 4..]),
        None => (raw, &raw[raw.len()..]),
    };
    let head = String::from_utf8_lossy(head);
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let mut content_type = None;
    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.eq_ignore_ascii_case("chunked")
            {
                chunked = true;
            }
        }
    }
    let body = if chunked {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    HttpResponse {
        status,
        content_type,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// Reassembles the complete chunks of a chunked body, ignoring a partial tail.
fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = rest.windows(2).position(|w| w == b"\r\n") {
        let size_line = std::str::from_utf8(&rest[..line_end]).unwrap_or("");
        let Ok(size) = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
        else {
            break;
        };
        let after = &rest[line_end + 2..];
        if size == 0 || after.len() < size {
            break;
        }
        out.extend_from_slice(&after[..size]);
        rest = after[size..]
            .strip_prefix(b"\r\n")
            .unwrap_or(&after[size..]);
    }
    out
}

/// Splits an SSE body into `{event, data}` records, parsing JSON data.
fn sse_events(body: &str) -> Vec<Value> {
    body.split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut data = Vec::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("data:") {
                    data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
                }
            }
            if event.is_none() && data.is_empty() {
                return None;
            }
            let data = data.join("\n");
            let data = serde_json::from_str(&data).unwrap_or(Value::String(data));
            Some(json!({"event": event, "data": data}))
        })
        .collect()
}

fn content_type_essence(content_type: Option<&str>) -> Value {
    content_type
        .map(|ct| Value::String(ct.split(';').next().unwrap_or(ct).trim().to_string()))
        .unwrap_or(Value::Null)
}

fn json_or_text(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_string()))
}

// ── WebSocket helpers ───────────────────────────────────────────────────

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(addr: SocketAddr, path: &str, protocol: Option<&str>) -> Ws {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{addr}{path}")
        .into_client_request()
        .expect("websocket request");
    if let Some(protocol) = protocol {
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            protocol.parse().expect("subprotocol header"),
        );
    }
    let (ws, _) = tokio::time::timeout(STEP_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .expect("websocket connect timed out")
        .expect("websocket upgrade");
    ws
}

async fn ws_send(ws: &mut Ws, transcript: &mut Transcript, channel: &str, payload: Value) {
    transcript.send(channel, payload.clone());
    ws.send(Message::Text(payload.to_string().into()))
        .await
        .expect("websocket send");
}

/// Next JSON text frame; pings, pongs, and binary frames are skipped.
async fn ws_next(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(STEP_TIMEOUT, ws.next())
            .await
            .expect("websocket frame timed out")
            .expect("websocket closed early")
            .expect("websocket read");
        if let Message::Text(text) = message {
            return json_or_text(text.as_str());
        }
    }
}

/// Records frames until one satisfies `last`, inclusive.
async fn ws_recv_until(
    ws: &mut Ws,
    transcript: &mut Transcript,
    channel: &str,
    last: impl Fn(&Value) -> bool,
) -> Value {
    loop {
        let frame = ws_next(ws).await;
        transcript.recv(channel, frame.clone());
        if last(&frame) {
            return frame;
        }
    }
}

fn ws_frame_type_is(types: &'static [&'static str]) -> impl Fn(&Value) -> bool {
    move |frame| {
        frame["type"]
            .as_str()
            .is_some_and(|kind| types.contains(&kind))
    }
}

async fn ws_chat_turn(addr: SocketAddr, mut t: Transcript, session: &str) -> Transcript {
    let path = format!("/ws/chat?agent={AGENT}&session_id={session}");
    t.send("ws", json!({"connect": path}));
    let mut ws = ws_connect(addr, &path, None).await;
    ws_recv_until(&mut ws, &mut t, "ws", ws_frame_type_is(&["session_start"])).await;
    ws_send(&mut ws, &mut t, "ws", json!({"type": "connect"})).await;
    ws_recv_until(&mut ws, &mut t, "ws", ws_frame_type_is(&["connected"])).await;
    ws_send(
        &mut ws,
        &mut t,
        "ws",
        json!({"type": "message", "content": "Say hello to the golden frames."}),
    )
    .await;
    ws_recv_until(&mut ws, &mut t, "ws", ws_frame_type_is(&["done", "error"])).await;
    let _ = ws.close(None).await;
    t
}

// ── Scenarios ───────────────────────────────────────────────────────────

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn ws_chat_turn_completes() {
    run_scenario(
        "ws_chat_turn_completes",
        vec![Reply::Text("Hello, golden frames.")],
        |addr, t| async move { ws_chat_turn(addr, t, "golden-ws-turn").await },
    );
}

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn ws_chat_provider_error() {
    run_scenario(
        "ws_chat_provider_error",
        vec![Reply::Status(500)],
        |addr, t| async move { ws_chat_turn(addr, t, "golden-ws-error").await },
    );
}

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn webhook_json_turn() {
    run_scenario(
        "webhook_json_turn",
        vec![Reply::Text("Webhook reply from the fixture.")],
        |addr, mut t| async move {
            let path = format!("/webhook?agent={AGENT}");
            let body = json!({"message": "Reply to this webhook."});
            let headers = [("X-Session-Id", "golden-webhook-json")];
            t.send("http", json!({"method": "POST", "path": path, "headers": headers_json(&headers), "body": body}));
            let response = http_request(addr, "POST", &path, &headers, Some(&body)).await;
            t.recv(
                "http",
                json!({
                    "status": response.status,
                    "content_type": content_type_essence(response.content_type.as_deref()),
                    "body": json_or_text(&response.body),
                }),
            );
            t
        },
    );
}

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn webhook_sse_turn() {
    run_scenario(
        "webhook_sse_turn",
        vec![Reply::Text("Streamed webhook reply.")],
        |addr, mut t| async move {
            let path = format!("/webhook?agent={AGENT}");
            let body = json!({"message": "Stream a reply.", "stream": true});
            let headers = [
                ("Accept", "text/event-stream"),
                ("X-Session-Id", "golden-webhook-sse"),
            ];
            t.send("http", json!({"method": "POST", "path": path, "headers": headers_json(&headers), "body": body}));
            let response = http_request(addr, "POST", &path, &headers, Some(&body)).await;
            t.recv(
                "http",
                json!({
                    "status": response.status,
                    "content_type": content_type_essence(response.content_type.as_deref()),
                }),
            );
            for event in sse_events(&response.body) {
                t.recv("sse", event);
            }
            t
        },
    );
}

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn webhook_rejects_unknown_agent() {
    run_scenario(
        "webhook_rejects_unknown_agent",
        vec![Reply::Text("unused")],
        |addr, mut t| async move {
            let path = "/webhook?agent=no-such-agent".to_string();
            let body = json!({"message": "Route to a missing agent."});
            t.send(
                "http",
                json!({"method": "POST", "path": path, "body": body}),
            );
            let response = http_request(addr, "POST", &path, &[], Some(&body)).await;
            t.recv(
                "http",
                json!({
                    "status": response.status,
                    "content_type": content_type_essence(response.content_type.as_deref()),
                    "body": json_or_text(&response.body),
                }),
            );
            t
        },
    );
}

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn acp_session_prompt() {
    run_scenario(
        "acp_session_prompt",
        vec![Reply::Text("ACP reply from the fixture.")],
        |addr, mut t| async move {
            let path = format!("/acp?agent={AGENT}");
            t.send(
                "acp",
                json!({"connect": path, "protocol": "zeroclaw.acp.v1"}),
            );
            let mut ws = ws_connect(addr, &path, Some("zeroclaw.acp.v1")).await;
            let is_response = |id: i64| move |frame: &Value| frame["id"] == json!(id);

            ws_send(
                &mut ws,
                &mut t,
                "acp",
                json!({"jsonrpc": "2.0", "id": 1,
                "method": "initialize", "params": {"protocolVersion": 1}}),
            )
            .await;
            ws_recv_until(&mut ws, &mut t, "acp", is_response(1)).await;

            ws_send(
                &mut ws,
                &mut t,
                "acp",
                json!({"jsonrpc": "2.0", "id": 2,
                "method": "session/new", "params": {"agentAlias": AGENT}}),
            )
            .await;
            let created = ws_recv_until(&mut ws, &mut t, "acp", is_response(2)).await;
            let session_id = created["result"]["sessionId"]
                .as_str()
                .expect("session/new returns a sessionId")
                .to_string();

            ws_send(
                &mut ws,
                &mut t,
                "acp",
                json!({"jsonrpc": "2.0", "id": 3,
                "method": "session/prompt", "params": {"sessionId": session_id,
                "prompt": [{"type": "text", "text": "Say hello over ACP."}]}}),
            )
            .await;
            ws_recv_until(&mut ws, &mut t, "acp", is_response(3)).await;
            let _ = ws.close(None).await;
            t
        },
    );
}

#[test]
#[ignore = "golden-frame replay runs in the informational CI step"]
fn events_stream_observes_webhook_turn() {
    run_scenario(
        "events_stream_observes_webhook_turn",
        vec![Reply::Text("Observed reply.")],
        |addr, mut t| async move {
            t.send("sse", json!({"method": "GET", "path": "/api/events"}));
            let mut events_stream = open_request(
                addr,
                "GET",
                "/api/events",
                &[("Accept", "text/event-stream")],
                None,
            )
            .await;
            // The subscription exists once the response head has arrived.
            let head = read_response_head(&mut events_stream).await;

            let path = format!("/webhook?agent={AGENT}");
            let body = json!({"message": "Trigger an observed turn."});
            let headers = [("X-Session-Id", "golden-events")];
            t.send("http", json!({"method": "POST", "path": path, "headers": headers_json(&headers), "body": body}));
            let response = http_request(addr, "POST", &path, &headers, Some(&body)).await;
            t.recv(
                "http",
                json!({"status": response.status, "body": json_or_text(&response.body)}),
            );

            let raw = read_response(&mut events_stream, head, Some("\"agent_end\"")).await;
            let events = parse_http_response(&raw);
            t.recv(
                "sse",
                json!({
                    "status": events.status,
                    "content_type": content_type_essence(events.content_type.as_deref()),
                }),
            );
            for event in sse_events(&events.body) {
                if is_turn_event(&event) {
                    t.recv("sse", event);
                }
            }
            t
        },
    );
}

/// Keeps agent-lifecycle events from `/api/events`; drops log lines and
/// keepalives, whose count and order depend on timing.
fn is_turn_event(event: &Value) -> bool {
    let kind = event["data"]["type"].as_str().unwrap_or_default();
    matches!(
        kind,
        "agent_start" | "llm_request" | "llm_response" | "tool_call" | "tool_result" | "agent_end"
    )
}

fn headers_json(headers: &[(&str, &str)]) -> Value {
    Value::Object(
        headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect::<serde_json::Map<_, _>>(),
    )
}

// ── Plugin webhook scenarios ────────────────────────────────────────────

/// `/plugin/{path}` scenarios. Each exchange records the request, then on the
/// `plugin` channel what the route's worker received, then the response. An
/// exchange the gateway answers on its own has no `plugin` frame, which pins
/// that the request never reached a worker. Bodies and header values are
/// recorded as text when they are UTF-8 and as hex otherwise, so a fixture
/// pins the exact bytes a worker receives. The last frame lists every worker
/// report no exchange claimed, collected once the routes are retired and every
/// worker has stopped, so an extra or duplicate delivery is a fixture diff.
#[cfg(feature = "plugins-wasm")]
mod plugin_webhook {
    use super::*;

    use std::time::Instant;

    use tokio::sync::mpsc;
    use zeroclaw_api::webhook::{
        MAX_WEBHOOK_RESPONSE_BODY_BYTES, PLUGIN_WEBHOOK_DEADLINE, PluginWebhookOwner,
        PluginWebhookRegistry, PluginWebhookRegistryLease, PluginWebhookRoute, RawWebhook,
        WebhookCancellation, WebhookOutcome, WebhookReject, WebhookReservation,
    };
    use zeroclaw_infra::plugin_webhook::PluginWebhookIngress;

    /// Queue depth of the worker-backed routes; none of them fills up.
    const WORKER_QUEUE: usize = 4;
    /// A capacity-1 route whose queue is filled before the gateway starts and
    /// never drained.
    const FULL_ROUTE: &str = "queue-full";
    /// A route whose receiver is dropped before the gateway starts.
    const CLOSED_ROUTE: &str = "closed";
    /// The header carrying the message ID the `dedup` worker reserves.
    const MESSAGE_ID_HEADER: &str = "x-golden-message-id";

    /// How the worker behind a route answers each request.
    #[derive(Clone, Copy)]
    enum Worker {
        Ack,
        EchoQuery,
        /// Replies with the `challenge` field of a JSON body.
        EchoChallenge,
        EmptyReply,
        /// Reserves the message ID through the request's idempotency bridge,
        /// commits it when it owns the reservation, and acknowledges.
        Dedup,
        Unauthorized,
        BadRequest,
        InvalidResponse,
        /// Replies one byte over the reply bound.
        OversizedReply,
        Unavailable,
        TimeoutReject,
        /// Drops the reply sender without answering.
        DropReply,
        /// Never answers, and reports when the gateway cancels the request.
        Stall,
    }

    const ROUTES: [(&str, Worker); 13] = [
        ("ack", Worker::Ack),
        ("echo-query", Worker::EchoQuery),
        ("echo-challenge", Worker::EchoChallenge),
        ("empty-reply", Worker::EmptyReply),
        ("dedup", Worker::Dedup),
        ("unauthorized", Worker::Unauthorized),
        ("bad-request", Worker::BadRequest),
        ("invalid-response", Worker::InvalidResponse),
        ("oversized-reply", Worker::OversizedReply),
        ("unavailable", Worker::Unavailable),
        ("timeout-reject", Worker::TimeoutReject),
        ("dropped-reply", Worker::DropReply),
        ("stalled", Worker::Stall),
    ];

    /// Serves one route. Every request is reported before it is answered, so
    /// the report is queued by the time the gateway responds.
    async fn serve(
        route: &'static str,
        worker: Worker,
        mut requests: mpsc::Receiver<RawWebhook>,
        reports: mpsc::UnboundedSender<Value>,
    ) {
        while let Some(request) = requests.recv().await {
            let mut report = json!({
                "route": route,
                "method": request.method,
                "query": request.query,
                "headers": request.headers,
                "body": text_or_hex(&request.body),
            });
            let answer = match worker {
                Worker::Ack => Ok(WebhookOutcome::Ack),
                Worker::EchoQuery => Ok(WebhookOutcome::Body(request.query.clone())),
                Worker::EchoChallenge => Ok(WebhookOutcome::Body(challenge(&request.body))),
                Worker::EmptyReply => Ok(WebhookOutcome::Body(String::new())),
                Worker::Dedup => {
                    report["reservation"] = json!(reserve(&request));
                    Ok(WebhookOutcome::Ack)
                }
                Worker::Unauthorized => Err(WebhookReject::Unauthorized(
                    "private signature detail".to_string(),
                )),
                Worker::BadRequest => Err(WebhookReject::BadRequest(
                    "private parser detail".to_string(),
                )),
                Worker::InvalidResponse => Err(WebhookReject::InvalidResponse),
                Worker::OversizedReply => Ok(WebhookOutcome::Body(
                    "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES + 1),
                )),
                Worker::Unavailable => Err(WebhookReject::Unavailable(
                    "private host detail".to_string(),
                )),
                Worker::TimeoutReject => Err(WebhookReject::Timeout),
                Worker::DropReply => {
                    let _ = reports.send(report);
                    continue;
                }
                Worker::Stall => {
                    let _ = reports.send(report);
                    request.cancellation.cancelled().await;
                    let _ = reports.send(json!({"route": route, "cancelled": true}));
                    continue;
                }
            };
            let _ = reports.send(report);
            let _ = request.reply.send(answer);
        }
    }

    fn challenge(body: &[u8]) -> String {
        serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|body| body["challenge"].as_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// Reserves the request's message ID the way a channel worker does once
    /// it has delivered the message.
    fn reserve(request: &RawWebhook) -> &'static str {
        let Some(idempotency) = &request.idempotency else {
            return "no idempotency bridge";
        };
        let message_id = request
            .headers
            .iter()
            .find(|(name, _)| name == MESSAGE_ID_HEADER)
            .map_or("", |(_, value)| value.as_str());
        match idempotency.begin(message_id) {
            WebhookReservation::Owner(token) => {
                if idempotency.commit(&token) {
                    "owner, committed"
                } else {
                    "owner, commit refused"
                }
            }
            WebhookReservation::Committed => "committed",
            WebhookReservation::InFlight(_) => "in flight",
            WebhookReservation::Unavailable => "unavailable",
        }
    }

    /// The route generation a channel supervisor would publish, served by
    /// scripted workers. The routes stay live while this value does.
    struct ScriptedRoutes {
        _lease: PluginWebhookRegistryLease,
        _full_queue: mpsc::Receiver<RawWebhook>,
    }

    impl ScriptedRoutes {
        fn publish(
            registry: &PluginWebhookRegistry,
            reports: &mpsc::UnboundedSender<Value>,
        ) -> Self {
            let route_for = |sink| {
                PluginWebhookRoute::new(PluginWebhookOwner::new("golden-plugin", "golden"), sink)
            };
            let mut routes = HashMap::new();
            for (route, worker) in ROUTES {
                let (sink, requests) = mpsc::channel(WORKER_QUEUE);
                routes.insert(route.to_string(), route_for(sink));
                let task = serve(route, worker, requests, reports.clone());
                zeroclaw_spawn::spawn!(task);
            }

            let (full_sink, full_queue) = mpsc::channel(1);
            let (unanswered, _) = tokio::sync::oneshot::channel();
            full_sink
                .try_send(RawWebhook {
                    method: "POST".to_string(),
                    query: String::new(),
                    headers: Vec::new(),
                    body: Vec::new(),
                    cancellation: WebhookCancellation::new(),
                    idempotency: None,
                    reply: unanswered,
                })
                .expect("fill the full route's queue");
            routes.insert(FULL_ROUTE.to_string(), route_for(full_sink));

            let (closed_sink, closed_requests) = mpsc::channel(1);
            drop(closed_requests);
            routes.insert(CLOSED_ROUTE.to_string(), route_for(closed_sink));

            let lease = registry.start_generation();
            assert!(
                lease.replace(routes),
                "the scripted generation owns the registry"
            );
            Self {
                _lease: lease,
                _full_queue: full_queue,
            }
        }
    }

    /// Whether a request reaches a route's worker or the gateway answers it
    /// on its own.
    #[derive(Clone, Copy)]
    enum Reaches {
        Worker,
        GatewayOnly,
    }

    struct Client {
        addr: SocketAddr,
        reports: mpsc::UnboundedReceiver<Value>,
        transcript: Transcript,
    }

    impl Client {
        async fn exchange(
            &mut self,
            method: &str,
            path: &str,
            headers: &[(&str, &[u8])],
            body: &[u8],
            reaches: Reaches,
        ) {
            self.transcript.send(
                "http",
                json!({
                    "method": method,
                    "path": path,
                    "headers": raw_headers_json(headers),
                    "body": text_or_hex(body),
                }),
            );
            let mut request = request_head(self.addr, method, path, headers, body.len());
            request.extend_from_slice(body);
            self.complete(&request, reaches).await;
        }

        /// Declares a `body_len`-byte body and withholds it. The body limit
        /// answers from the declared length; a body the server never reads
        /// would make its close reset the connection and race the response.
        async fn exchange_withholding_body(&mut self, method: &str, path: &str, body_len: usize) {
            self.transcript.send(
                "http",
                json!({"method": method, "path": path, "declared_body_bytes": body_len}),
            );
            let request = request_head(self.addr, method, path, &[], body_len);
            self.complete(&request, Reaches::GatewayOnly).await;
        }

        async fn complete(&mut self, request: &[u8], reaches: Reaches) {
            let mut stream = write_request(self.addr, request).await;
            let raw = read_response(&mut stream, Vec::new(), None).await;
            match reaches {
                Reaches::Worker => self.observe().await,
                // Anything queued here reached a worker it should not have,
                // and shows up as a fixture diff.
                Reaches::GatewayOnly => {
                    while let Ok(report) = self.reports.try_recv() {
                        self.transcript.recv("plugin", report);
                    }
                }
            }
            let response = parse_http_response(&raw);
            let mut payload = json!({
                "status": response.status,
                "content_type": response.content_type,
                "body": response.body,
            });
            if let Some(allow) = response_header(&raw, "allow") {
                payload["allow"] = Value::String(allow);
            }
            self.transcript.recv("http", payload);
        }

        /// Records the next worker report.
        async fn observe(&mut self) {
            let report = tokio::time::timeout(STEP_TIMEOUT, self.reports.recv())
                .await
                .expect("a worker should report")
                .expect("workers outlive the scenario");
            self.transcript.recv("plugin", report);
        }
    }

    /// Header pairs in send order, each value as [`text_or_hex`] renders it.
    fn raw_headers_json(headers: &[(&str, &[u8])]) -> Value {
        headers
            .iter()
            .map(|(name, value)| json!([name, text_or_hex(value)]))
            .collect()
    }

    /// `bytes` as text when they are UTF-8, and as `<bytes HEX>` otherwise:
    /// a lossy decode would map different invalid bytes to the same text.
    fn text_or_hex(bytes: &[u8]) -> String {
        std::str::from_utf8(bytes).map_or_else(
            |_| format!("<bytes {}>", hex::encode(bytes)),
            str::to_string,
        )
    }

    fn response_header(raw: &[u8], name: &str) -> Option<String> {
        let end = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
        String::from_utf8_lossy(&raw[..end])
            .lines()
            .skip(1)
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
    }

    /// Starts the gateway with an ingress built from the scenario's config,
    /// as the daemon builds one per generation, and publishes the scripted
    /// routes into it.
    async fn start_gateway(
        provider: &ScriptedProvider,
        configure: fn(&mut Config),
        reports: &mpsc::UnboundedSender<Value>,
    ) -> (Gateway, ScriptedRoutes) {
        let root = tempfile::TempDir::new().expect("gateway temp root");
        let mut config = fixture_config(root.path(), &provider.base_url());
        configure(&mut config);
        let authority = zeroclaw_runtime::LiveConfigAuthority::new_owned(config.clone())
            .expect("scenario config builds a live config authority");
        let ingress = Arc::new(PluginWebhookIngress::new(
            config.gateway.idempotency_ttl_secs,
            config.gateway.idempotency_max_keys,
        ));
        let routes = ScriptedRoutes::publish(ingress.registry(), reports);
        let gateway = Gateway::launch(root, move |port, reload_controls, readiness| {
            Box::pin(zeroclaw_gateway::run_gateway_with_plugin_webhooks(
                "127.0.0.1",
                port,
                config,
                None,
                Some(reload_controls),
                None,
                None,
                None,
                None,
                None,
                zeroclaw_gateway::GatewaySupervision::new(
                    Some(readiness),
                    ingress,
                    authority,
                    None,
                ),
            ))
        })
        .await;
        (gateway, routes)
    }

    /// Runs a scenario against a supervised gateway whose plugin webhook
    /// routes are served by scripted workers. The agent config still names
    /// the scripted provider; no plugin webhook reaches it.
    fn run_plugin_scenario<F, Fut>(scenario: &'static str, configure: fn(&mut Config), body: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Client>,
    {
        on_scenario_runtime(scenario, move || async move {
            let provider = ScriptedProvider::spawn(vec![Reply::Text("unused")]).await;
            let (report_tx, reports) = mpsc::unbounded_channel();
            let (gateway, routes) = start_gateway(&provider, configure, &report_tx).await;
            drop(report_tx);
            let client = Client {
                addr: gateway.addr,
                reports,
                transcript: Transcript::default(),
            };
            let Client {
                reports,
                transcript,
                ..
            } = tokio::time::timeout(Duration::from_secs(60), body(client))
                .await
                .unwrap_or_else(|_| panic!("scenario `{scenario}` timed out"));
            let mut normalizer = Normalizer::new(&gateway, &provider);
            let mut frames: Vec<Value> = transcript
                .frames
                .iter()
                .map(|frame| normalizer.value(None, frame))
                .collect();
            gateway.stop().await;
            drop(routes);
            let unclaimed: Vec<Value> = unclaimed_reports(reports)
                .await
                .iter()
                .map(|report| normalizer.value(None, report))
                .collect();
            frames.push(json!({"dir": "meta", "channel": "plugin",
                "payload": {"unclaimed_reports": unclaimed}}));
            check_fixture(scenario, frames);
        });
    }

    /// Every report still queued once the workers have stopped. Retiring the
    /// routes drops the last sink of each worker's queue, so each worker
    /// serves what it already holds, exits, and drops its report sender; the
    /// channel closes once every report is in. One worker reports in the order
    /// it served, but two workers' reports interleave by scheduling, so they
    /// are grouped by route with a stable sort.
    async fn unclaimed_reports(mut reports: mpsc::UnboundedReceiver<Value>) -> Vec<Value> {
        let mut unclaimed = Vec::new();
        while let Some(report) = tokio::time::timeout(STEP_TIMEOUT, reports.recv())
            .await
            .expect("workers stop once their routes are retired")
        {
            unclaimed.push(report);
        }
        unclaimed.sort_by(|a, b| a["route"].as_str().cmp(&b["route"].as_str()));
        unclaimed
    }

    #[test]
    fn delivery() {
        run_plugin_scenario(
            "plugin_webhook_delivery",
            |_| {},
            |mut client| async move {
                // Header names reach the worker lowercased, a repeated name
                // keeps every value, and a value outside visible ASCII is
                // dropped, whether raw non-UTF-8 bytes or non-ASCII UTF-8.
                client
                    .exchange(
                        "POST",
                        "/plugin/ack?source=golden&n=1",
                        &[
                            ("Content-Type", "text/plain; charset=utf-8".as_bytes()),
                            ("X-Golden-Signature", "sha256=0f1e2d".as_bytes()),
                            ("X-Golden-Multi", "one".as_bytes()),
                            ("X-Golden-Multi", "two".as_bytes()),
                            ("X-Golden-Opaque", b"\xff\xfe".as_slice()),
                            ("X-Golden-Accent", "caf\u{e9}".as_bytes()),
                        ],
                        b"plain text body, not JSON",
                        Reaches::Worker,
                    )
                    .await;
                // The body reaches the worker byte for byte, including a NUL
                // and bytes that are not UTF-8.
                client
                    .exchange(
                        "POST",
                        "/plugin/ack",
                        &[("Content-Type", "application/octet-stream".as_bytes())],
                        b"\x00\xff\xfe opaque \x80 bytes\r\n",
                        Reaches::Worker,
                    )
                    .await;
                client
                    .exchange(
                        "GET",
                        "/plugin/echo-query?challenge=a%2Bb&part=one&part=two",
                        &[],
                        b"",
                        Reaches::Worker,
                    )
                    .await;
                client
                    .exchange(
                        "POST",
                        "/plugin/echo-challenge",
                        &[("Content-Type", "application/json".as_bytes())],
                        br#"{"type":"url_verification","challenge":"golden-challenge"}"#,
                        Reaches::Worker,
                    )
                    .await;
                client
                    .exchange("GET", "/plugin/empty-reply", &[], b"", Reaches::Worker)
                    .await;
                // The second delivery of one message ID finds it committed.
                for _ in 0..2 {
                    client
                        .exchange(
                            "POST",
                            "/plugin/dedup",
                            &[(MESSAGE_ID_HEADER, "golden-message-1".as_bytes())],
                            b"{}",
                            Reaches::Worker,
                        )
                        .await;
                }
                client
            },
        );
    }

    #[test]
    fn rejections() {
        run_plugin_scenario(
            "plugin_webhook_rejections",
            |_| {},
            |mut client| async move {
                for route in [
                    "unauthorized",
                    "bad-request",
                    "invalid-response",
                    "oversized-reply",
                    "unavailable",
                    "timeout-reject",
                    "dropped-reply",
                ] {
                    client
                        .exchange(
                            "POST",
                            &format!("/plugin/{route}"),
                            &[],
                            b"signed payload",
                            Reaches::Worker,
                        )
                        .await;
                }
                client
            },
        );
    }

    #[test]
    fn admission() {
        run_plugin_scenario(
            "plugin_webhook_admission",
            |_| {},
            |mut client| async move {
                for route in ["missing", "not.a.route", FULL_ROUTE, CLOSED_ROUTE] {
                    client
                        .exchange(
                            "POST",
                            &format!("/plugin/{route}"),
                            &[],
                            b"body",
                            Reaches::GatewayOnly,
                        )
                        .await;
                }
                // The method check comes before route lookup: an unknown path is
                // refused the same way as a live one.
                client
                    .exchange("HEAD", "/plugin/ack", &[], b"", Reaches::GatewayOnly)
                    .await;
                client
                    .exchange("PUT", "/plugin/missing", &[], b"", Reaches::GatewayOnly)
                    .await;
                client
                    .exchange_withholding_body(
                        "POST",
                        "/plugin/ack",
                        zeroclaw_gateway::MAX_BODY_SIZE + 1,
                    )
                    .await;
                client
            },
        );
    }

    #[test]
    fn rate_limit() {
        run_plugin_scenario(
            "plugin_webhook_rate_limit",
            |config| config.gateway.webhook_rate_limit_per_minute = 1,
            |mut client| async move {
                client
                    .exchange("POST", "/plugin/ack", &[], b"first", Reaches::Worker)
                    .await;
                client
                    .exchange("POST", "/plugin/ack", &[], b"second", Reaches::GatewayOnly)
                    .await;
                // The limit applies before route lookup.
                client
                    .exchange(
                        "POST",
                        "/plugin/missing",
                        &[],
                        b"third",
                        Reaches::GatewayOnly,
                    )
                    .await;
                client
            },
        );
    }

    #[test]
    fn timeout() {
        run_plugin_scenario(
            "plugin_webhook_timeout",
            |_| {},
            |mut client| async move {
                let started = Instant::now();
                client
                    .exchange(
                        "POST",
                        "/plugin/stalled",
                        &[],
                        b"never answered",
                        Reaches::Worker,
                    )
                    .await;
                assert!(
                    started.elapsed() >= PLUGIN_WEBHOOK_DEADLINE,
                    "the gateway answered before its deadline"
                );
                // Giving up cancels the request the worker still holds.
                client.observe().await;
                client
            },
        );
    }
}

// ── Harness self-tests (not ignored: no gateway, no network) ────────────

const SAMPLE_UUID: &str = "123e4567-e89b-12d3-a456-426614174000";

#[test]
fn normalizer_survives_multibyte_text_at_the_uuid_width() {
    let mut normalizer = Normalizer::with_replacements(Vec::new());
    // Every prefix length puts a multibyte character on each side of the
    // 36-byte window at some point, including 35 ASCII bytes then `é`.
    for prefix in 0..40 {
        for tail in ["é", "日本", "🦀", "aé", "é\u{301}"] {
            let text = format!("{}{tail}{}", "a".repeat(prefix), "b".repeat(prefix % 3));
            assert_eq!(normalizer.string(&text), text, "unchanged: {text:?}");
        }
    }
}

#[test]
fn normalizer_numbers_uuids_by_first_appearance_beside_multibyte_text() {
    let mut normalizer = Normalizer::with_replacements(Vec::new());
    let other = "00000000-0000-0000-0000-000000000000";
    let upper = SAMPLE_UUID.to_ascii_uppercase();
    let text = format!("é{SAMPLE_UUID}日 {other} 🦀{upper}");
    assert_eq!(normalizer.string(&text), "é<uuid:1>日 <uuid:2> 🦀<uuid:1>");
    // Numbering persists across strings in one transcript.
    assert_eq!(normalizer.string(other), "<uuid:2>");
    // A 36-byte run that is not a UUID is left alone.
    let near_miss = "123e4567-e89b-12d3-a456_426614174000";
    assert_eq!(normalizer.string(near_miss), near_miss);
}

#[test]
fn normalizer_masks_timing_and_clock_fields_only() {
    let mut normalizer = Normalizer::with_replacements(vec![("/tmp/x".into(), "<root>".into())]);
    let input = json!({
        "duration_ms": 12,
        "timestamp": 1_700_000_000,
        "input_tokens": 10,
        "at": "2026-09-26T12:00:00Z",
        "path": "/tmp/x/workspace",
    });
    assert_eq!(
        normalizer.value(None, &input),
        json!({
            "duration_ms": "<duration>",
            "timestamp": "<timestamp>",
            "input_tokens": 10,
            "at": "<timestamp>",
            "path": "<root>/workspace",
        })
    );
}

#[test]
fn coalescing_joins_only_adjacent_received_ws_chunks() {
    let chunk = |content: &str| json!({"dir": "recv", "channel": "ws", "payload": {"type": "chunk", "content": content}});
    let done = json!({"dir": "recv", "channel": "ws", "payload": {"type": "done"}});
    let frames = vec![
        chunk("Hel"),
        chunk("lo, "),
        chunk("é"),
        done.clone(),
        chunk("again"),
    ];
    assert_eq!(
        coalesce_chunks(frames),
        vec![chunk("Hello, é"), done, chunk("again")]
    );
}

#[test]
fn chunked_body_reassembles_multibyte_text_split_across_chunks() {
    let text = "héllo 日本";
    let bytes = text.as_bytes();
    // Split inside `é` (2 bytes) and inside `日` (3 bytes).
    let (a, rest) = bytes.split_at(2);
    let (b, c) = rest.split_at(6);
    let mut raw =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n"
            .to_vec();
    for part in [a, b, c] {
        raw.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        raw.extend_from_slice(part);
        raw.extend_from_slice(b"\r\n");
    }
    let complete = raw.len();
    raw.extend_from_slice(b"0\r\n\r\n");

    let parsed = parse_http_response(&raw);
    assert_eq!(parsed.status, 200);
    assert_eq!(parsed.content_type.as_deref(), Some("text/plain"));
    assert_eq!(parsed.body, text);

    // A partial final chunk is ignored rather than decoded half-way.
    let mut partial = raw[..complete].to_vec();
    partial.extend_from_slice(b"5\r\nab");
    assert_eq!(parse_http_response(&partial).body, text);
}
