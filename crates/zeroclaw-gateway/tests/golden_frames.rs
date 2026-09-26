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

        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe free port");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);

        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (reload_tx, _) = tokio::sync::watch::channel(false);
        let reload_controls = zeroclaw_runtime::daemon::GatewayReloadControls {
            shutdown_tx: shutdown_tx.clone(),
            reload_tx,
        };
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(None);
        let readiness = zeroclaw_runtime::daemon::GatewayReadinessReporter::new(move |addr| {
            let _ = ready_tx.send(Some(addr));
        });
        let server = zeroclaw_spawn::spawn!(async move {
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
            .await
        });
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
            if i + 36 <= bytes.len() && is_uuid(&s[i..i + 36]) {
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

fn is_uuid(candidate: &str) -> bool {
    candidate.len() == 36
        && candidate.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
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

/// Runs a scenario on a runtime with large worker stacks: building a
/// runtime agent overflows the default test-thread stack on Linux.
fn run_scenario<F, Fut>(scenario: &'static str, script: Vec<Reply>, body: F)
where
    F: FnOnce(SocketAddr, Transcript) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Transcript>,
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
            runtime.block_on(async move {
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
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to gateway");
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if body.is_some() {
        request.push_str("Content-Type: application/json\r\n");
    }
    request.push_str(&format!(
        "Content-Length: {}\r\n\r\n{payload}",
        payload.len()
    ));
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
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
    let text = String::from_utf8_lossy(raw);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
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
        rest.to_string()
    };
    HttpResponse {
        status,
        content_type,
        body,
    }
}

/// Decodes the complete chunks of a chunked body, ignoring a partial tail.
fn dechunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
        else {
            break;
        };
        if size == 0 || after.len() < size {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
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
