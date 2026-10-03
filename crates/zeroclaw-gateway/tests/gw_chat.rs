//! The dashboard chat socket through the separate gateway: the core's real
//! local listener in this test process, the `zeroclaw-gw` binary as a child
//! that reaches it only over the socket, and a scripted model provider. The
//! core runs every turn; the gateway translates frames. Unix only: the
//! preview refuses to start elsewhere.
#![cfg(unix)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::multi_agent::{AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_runtime::rpc::context::RpcContext;

const TOKEN: &str = "zc_chat_token";
/// A second paired device: the same operator, its own core connection.
const OTHER_TOKEN: &str = "zc_chat_other_token";
const AGENT: &str = "web";
const WAIT: Duration = Duration::from_secs(30);
/// A reply that streams for about 20 s, so a turn is surely still running
/// when a test stops it, however loaded the machine.
const LONG_REPLY: &str = "one two three four five six seven eight nine ten eleven twelve \
    thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty twenty-one \
    twenty-two twenty-three twenty-four twenty-five twenty-six twenty-seven twenty-eight \
    twenty-nine thirty thirty-one thirty-two thirty-three thirty-four thirty-five \
    thirty-six thirty-seven thirty-eight thirty-nine forty forty-one forty-two \
    forty-three forty-four forty-five forty-six forty-seven forty-eight forty-nine fifty \
    fifty-one fifty-two fifty-three fifty-four fifty-five fifty-six fifty-seven \
    fifty-eight fifty-nine sixty sixty-one sixty-two sixty-three sixty-four sixty-five \
    sixty-six sixty-seven sixty-eight sixty-nine seventy seventy-one seventy-two \
    seventy-three seventy-four seventy-five seventy-six seventy-seven seventy-eight \
    seventy-nine eighty eighty-one eighty-two eighty-three eighty-four eighty-five \
    eighty-six eighty-seven eighty-eight eighty-nine ninety ninety-one ninety-two \
    ninety-three ninety-four ninety-five ninety-six ninety-seven ninety-eight \
    ninety-nine one-hundred";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ── Scripted model provider ─────────────────────────────────────────────

/// One scripted provider reply, consumed in order; the last one repeats.
#[derive(Clone)]
enum Reply {
    /// Assistant text, streamed word by word.
    Text(&'static str),
    /// Assistant text, streamed a word every 200 ms.
    Slow(&'static str),
    /// One call of a tool with JSON arguments.
    ToolCall(&'static str, &'static str),
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
                    provider_response(reply, stream)
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

fn provider_response(reply: Reply, stream: bool) -> Response {
    let usage = json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15});
    let (text, delay) = match reply {
        Reply::Status(code) => {
            let status = StatusCode::from_u16(code).expect("scripted status code");
            let body =
                json!({"error": {"message": "scripted provider failure", "type": "server_error"}});
            return (status, axum::Json(body)).into_response();
        }
        Reply::ToolCall(name, arguments) => {
            let call = json!({
                "id": "call_1",
                "type": "function",
                "function": {"name": name, "arguments": arguments},
            });
            if !stream {
                return axum::Json(json!({
                    "id": "chatcmpl-chat",
                    "object": "chat.completion",
                    "model": "fixture-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": null, "tool_calls": [call]},
                        "finish_reason": "tool_calls"
                    }],
                    "usage": usage,
                }))
                .into_response();
            }
            let mut indexed = call;
            indexed["index"] = json!(0);
            let first = json!({"id": "chatcmpl-chat", "model": "fixture-model",
                "choices": [{"index": 0, "delta": {"role": "assistant", "tool_calls": [indexed]}}]});
            let last = json!({"id": "chatcmpl-chat", "model": "fixture-model",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": usage});
            let body = format!("data: {first}\n\ndata: {last}\n\ndata: [DONE]\n\n");
            return (
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from(body),
            )
                .into_response();
        }
        Reply::Text(text) => (text, Duration::ZERO),
        Reply::Slow(text) => (text, Duration::from_millis(200)),
    };
    if !stream {
        return axum::Json(json!({
            "id": "chatcmpl-chat",
            "object": "chat.completion",
            "model": "fixture-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"
            }],
            "usage": usage,
        }))
        .into_response();
    }
    let words: Vec<String> = text.split_inclusive(' ').map(str::to_owned).collect();
    let last = json!({"id": "chatcmpl-chat", "model": "fixture-model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": usage});
    let chunks = futures_util::stream::unfold(
        (words.into_iter(), 0usize),
        move |(mut words, i)| async move {
            let word = words.next()?;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let delta = if i == 0 {
                json!({"role": "assistant", "content": word})
            } else {
                json!({"content": word})
            };
            let chunk = json!({"id": "chatcmpl-chat", "model": "fixture-model",
            "choices": [{"index": 0, "delta": delta}]});
            Some((
                Ok::<_, std::convert::Infallible>(format!("data: {chunk}\n\n")),
                (words, i + 1),
            ))
        },
    );
    let tail = futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(format!("data: {last}\n\ndata: [DONE]\n\n"))
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(chunks.chain(tail)),
    )
        .into_response()
}

// ── Configuration shared by the core and the in-process gateway ────────

/// One agent on the scripted provider. With `approval`, the agent may read
/// files in its workspace and every read asks first.
fn chat_config(root: &Path, provider_url: &str, approval: bool) -> Config {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).expect("chat workspace");
    std::fs::write(workspace.join("notes.txt"), "the notes").expect("notes file");
    let mut config = Config {
        data_dir: workspace.clone(),
        config_path: root.join("config.toml"),
        ..Config::default()
    };
    config.gateway.require_pairing = true;
    config.gateway.paired_tokens = vec![TOKEN.into(), OTHER_TOKEN.into()];
    config.memory.backend = "none".to_string();
    config.memory.auto_save = false;
    config.memory.response_cache_enabled = false;
    config.reliability.provider_retries = 0;
    config.reliability.provider_backoff_ms = 0;
    config.providers.models.custom.insert(
        "fixture".to_string(),
        CustomModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("chat-test-key".to_string()),
                uri: Some(provider_url.to_string()),
                model: Some("fixture-model".to_string()),
                temperature: Some(0.0),
                ..ModelProviderConfig::default()
            },
        },
    );
    let risk = if approval {
        RiskProfileConfig {
            allowed_tools: vec!["file_read".to_string()],
            always_ask: vec!["file_read".to_string()],
            ..RiskProfileConfig::default()
        }
    } else {
        RiskProfileConfig {
            allowed_tools: vec!["__chat_test_no_tools__".to_string()],
            ..RiskProfileConfig::default()
        }
    };
    config.risk_profiles.insert("fixture".to_string(), risk);
    config.runtime_profiles.insert(
        "fixture".to_string(),
        RuntimeProfileConfig {
            max_tool_iterations: 2,
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

// ── The core ────────────────────────────────────────────────────────────

/// The daemon's real local listener over a context that runs real turns
/// and keeps their transcripts.
struct Core {
    ctx: Arc<RpcContext>,
    endpoint: PathBuf,
    cancel: CancellationToken,
    _listener: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Core {
    async fn serve(config: Config) -> Self {
        let backend = zeroclaw_infra::make_session_backend(
            &config.data_dir,
            &config.channels.session_backend,
        )
        .expect("session store");
        let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(
            16,
            Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        ));
        let mut ctx = RpcContext::for_live_test(config, sessions);
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context has one owner");
            ctx.session_backend = Some(backend);
            ctx.event_tx = Some(tokio::sync::broadcast::channel(256).0);
        }
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
        tokio::time::timeout(WAIT, async {
            while tokio::net::UnixStream::connect(&endpoint).await.is_err() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the core listens");
        Self {
            ctx,
            endpoint,
            cancel,
            _listener: listener,
        }
    }

    /// The assistant text the core persisted for `session_id`.
    fn persisted_reply(&self, session_id: &str) -> Option<String> {
        let backend = self.ctx.session_backend.as_ref()?;
        backend
            .load(&format!("rpc_{session_id}"))
            .iter()
            .rev()
            .find(|message| message.role == "assistant")
            .map(|message| message.content.clone())
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

// ── The separate gateway ────────────────────────────────────────────────

struct Gateway {
    _child: Child,
    address: String,
}

impl Gateway {
    async fn spawn(endpoint: &Path) -> Self {
        Self::spawn_with_args(endpoint, &[]).await
    }

    async fn spawn_with_args(endpoint: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw-gw"))
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--socket")
            .arg(endpoint)
            .args(args)
            .env_remove("ZEROCLAW_SOCKET")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn zeroclaw-gw");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        tokio::time::timeout(WAIT, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("zeroclaw-gw reports readiness")
            .expect("read its stdout");
        let address = line
            .trim()
            .strip_prefix("READY http://")
            .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
            .to_owned();
        Self {
            _child: child,
            address,
        }
    }
}

// ── Clients ─────────────────────────────────────────────────────────────

/// Open the chat socket as the dashboard does: the token in the `bearer.`
/// subprotocol.
async fn open_chat(address: &str, session_id: &str) -> Socket {
    let url = format!("ws://{address}/ws/chat?agent={AGENT}&session_id={session_id}");
    let mut request = url.into_client_request().expect("chat request");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        format!("zeroclaw.v1, bearer.{TOKEN}")
            .parse()
            .expect("protocol header"),
    );
    let (socket, response) = tokio::time::timeout(WAIT, tokio_tungstenite::connect_async(request))
        .await
        .expect("the upgrade answers in time")
        .expect("the chat socket opens");
    assert_eq!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok()),
        Some("zeroclaw.v1")
    );
    socket
}

async fn send(socket: &mut Socket, frame: Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .expect("send a frame");
}

/// The next JSON frame, skipping pings.
async fn next_frame(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(WAIT, socket.next())
            .await
            .expect("a frame in time")
            .expect("the socket stays open")
            .expect("a readable frame");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("a JSON frame");
        }
    }
}

/// Frames up to and including the first whose type is in `until`.
async fn frames_until(socket: &mut Socket, until: &[&str]) -> Vec<Value> {
    let mut frames = Vec::new();
    loop {
        let frame = next_frame(socket).await;
        let done = frame["type"].as_str().is_some_and(|t| until.contains(&t));
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

fn streamed_text(frames: &[Value]) -> String {
    frames
        .iter()
        .filter(|f| f["type"] == "chunk")
        .filter_map(|f| f["content"].as_str())
        .collect()
}

/// A plain HTTP request with the bearer; returns the status and JSON body.
async fn http(address: &str, method: &str, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect to zeroclaw-gw");
    let auth = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.expect("send");
    let mut response = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut response))
        .await
        .expect("a response in time")
        .expect("read the response");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").expect("headers and body");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status code");
    let body = serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_owned()));
    (status, body)
}

// ── Scenarios ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_prompt_streams_and_completes_through_the_core() {
    let provider = ScriptedProvider::spawn(vec![Reply::Text("Hello from the core.")]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let mut chat = open_chat(&gateway.address, "chat-stream").await;
    let start = next_frame(&mut chat).await;
    assert_eq!(
        start,
        json!({"type": "session_start", "session_id": "chat-stream", "resumed": false, "message_count": 0})
    );
    send(
        &mut chat,
        json!({"type": "message", "content": "Say hello."}),
    )
    .await;
    let frames = frames_until(&mut chat, &["done", "error"]).await;
    let done = frames.last().unwrap();
    assert_eq!(done["type"], "done", "{frames:?}");
    assert_eq!(done["full_response"], "Hello from the core.");
    assert_eq!(streamed_text(&frames), "Hello from the core.");
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    drop(chat);

    // The core kept the session: a new socket resumes it.
    assert_eq!(
        core.persisted_reply("chat-stream").as_deref(),
        Some("Hello from the core.")
    );
    let mut again = open_chat(&gateway.address, "chat-stream").await;
    let start = next_frame(&mut again).await;
    assert_eq!(start["resumed"], true, "{start}");
    assert!(start["message_count"].as_u64().unwrap() > 0, "{start}");
}

#[tokio::test]
async fn the_stop_button_cancels_a_streaming_turn() {
    let provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let mut chat = open_chat(&gateway.address, "chat-cancel").await;
    next_frame(&mut chat).await;
    send(
        &mut chat,
        json!({"type": "message", "content": "Count slowly."}),
    )
    .await;
    let first = next_frame(&mut chat).await;
    assert_eq!(first["type"], "chunk", "{first}");

    // Steering is refused while the turn runs, and the turn carries on.
    send(&mut chat, json!({"type": "message", "content": "faster"})).await;
    let refused = frames_until(&mut chat, &["error"]).await;
    assert_eq!(refused.last().unwrap()["code"], "CAPABILITY_MISSING");

    let (status, body) = http(
        &gateway.address,
        "POST",
        "/api/sessions/chat-cancel/abort",
        Some(TOKEN),
    )
    .await;
    assert_eq!((status, body), (200, json!({"status": "aborted"})));
    let frames = frames_until(&mut chat, &["aborted", "done", "error"]).await;
    assert_eq!(frames.last().unwrap()["type"], "aborted", "{frames:?}");

    let (status, body) = http(
        &gateway.address,
        "POST",
        "/api/sessions/chat-cancel/abort",
        Some(TOKEN),
    )
    .await;
    assert_eq!(
        (status, body),
        (200, json!({"status": "no_active_response"}))
    );
    let (status, _) = http(
        &gateway.address,
        "POST",
        "/api/sessions/chat-cancel/abort",
        None,
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn a_tool_approval_round_trips_through_the_core() {
    let provider = ScriptedProvider::spawn(vec![
        Reply::ToolCall("file_read", r#"{"path": "notes.txt"}"#),
        Reply::Text("I read the notes."),
    ])
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), true)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let mut chat = open_chat(&gateway.address, "chat-approve").await;
    next_frame(&mut chat).await;
    send(
        &mut chat,
        json!({"type": "message", "content": "Read my notes."}),
    )
    .await;
    let asked = frames_until(&mut chat, &["approval_request", "done", "error"]).await;
    let request = asked.last().unwrap();
    assert_eq!(request["type"], "approval_request", "{asked:?}");
    assert_eq!(request["tool"], "file_read");
    let request_id = request["request_id"]
        .as_str()
        .expect("a request id")
        .to_owned();

    // A SOP approval is refused rather than recorded under the wrong approver.
    send(
        &mut chat,
        json!({"type": "approval_response", "kind": "sop", "run_id": "r-1", "decision": "approve"}),
    )
    .await;
    let refused = frames_until(&mut chat, &["error"]).await;
    assert_eq!(refused.last().unwrap()["code"], "CAPABILITY_MISSING");

    send(
        &mut chat,
        json!({"type": "approval_response", "request_id": request_id, "decision": "approve"}),
    )
    .await;
    let frames = frames_until(&mut chat, &["done", "error"]).await;
    assert_eq!(frames.last().unwrap()["type"], "done", "{frames:?}");
    assert_eq!(frames.last().unwrap()["full_response"], "I read the notes.");
    let result = frames
        .iter()
        .find(|f| f["type"] == "tool_result")
        .unwrap_or_else(|| panic!("a tool result: {frames:?}"));
    assert!(
        result["output"]
            .as_str()
            .unwrap_or_default()
            .contains("the notes"),
        "{result}"
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_client_that_leaves_mid_turn_does_not_stop_the_turn() {
    let provider = ScriptedProvider::spawn(vec![Reply::Slow("one two three four five six")]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let mut chat = open_chat(&gateway.address, "chat-leave").await;
    next_frame(&mut chat).await;
    send(&mut chat, json!({"type": "message", "content": "Count."})).await;
    assert_eq!(next_frame(&mut chat).await["type"], "chunk");
    chat.close(None).await.expect("close the socket");
    drop(chat);

    // The turn runs to its end in the core and is kept, as the in-process
    // socket's turn is.
    let reply = tokio::time::timeout(WAIT, async {
        loop {
            if let Some(reply) = core.persisted_reply("chat-leave") {
                return reply;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the turn finishes after its viewer left");
    assert_eq!(reply, "one two three four five six");
}

#[tokio::test]
async fn the_upgrade_is_refused_before_it_happens() {
    let provider = ScriptedProvider::spawn(vec![Reply::Text("unused")]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let refused = |query: String| {
        let address = gateway.address.clone();
        async move {
            let request = format!("ws://{address}/ws/chat?{query}")
                .into_client_request()
                .unwrap();
            match tokio_tungstenite::connect_async(request).await {
                Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                    let body = response
                        .body()
                        .as_deref()
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .unwrap_or_default();
                    (response.status(), body)
                }
                other => panic!("expected a refused upgrade, got {other:?}"),
            }
        }
    };
    // No credential: refused with the sign-in hint, nothing upgraded.
    let (status, body) = refused(format!("agent={AGENT}&session_id=x")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body.contains("hint"), "{body}");
    // No agent: refused as the in-process gateway refuses it.
    let (status, _) = refused(format!("token={TOKEN}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // An agent the core does not know: a 400 too, now from the core.
    let (status, body) = refused(format!("token={TOKEN}&agent=nobody&session_id=y")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("invalid_params") && body.contains("nobody"),
        "{body}"
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn another_device_of_the_operator_can_stop_the_turn() {
    let provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let mut chat = open_chat(&gateway.address, "chat-elsewhere").await;
    next_frame(&mut chat).await;
    send(
        &mut chat,
        json!({"type": "message", "content": "Count slowly."}),
    )
    .await;
    assert_eq!(next_frame(&mut chat).await["type"], "chunk");

    // Another paired device of the same operator reaches the core on its own
    // connection; the core lets the operator stop its own turn from there,
    // as the in-process gateway does for any paired device.
    let (status, body) = http(
        &gateway.address,
        "POST",
        "/api/sessions/chat-elsewhere/abort",
        Some(OTHER_TOKEN),
    )
    .await;
    assert_eq!((status, body), (200, json!({"status": "aborted"})));
    let frames = frames_until(&mut chat, &["aborted", "done", "error"]).await;
    assert_eq!(frames.last().unwrap()["type"], "aborted", "{frames:?}");
}

// ── Parity with the in-process gateway ──────────────────────────────────

/// The in-process gateway over the same configuration, as the daemon runs
/// it.
async fn in_process_gateway(config: Config) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
    let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe free port");
    let port = probe.local_addr().expect("probe address").port();
    drop(probe);
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let (reload_tx, _) = tokio::sync::watch::channel(false);
    let reload_controls =
        zeroclaw_runtime::daemon::GatewayReloadControls::standalone(shutdown_tx.clone(), reload_tx);
    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(None);
    let readiness = zeroclaw_runtime::daemon::GatewayReadinessReporter::new(move |addr| {
        let _ = ready_tx.send(Some(addr));
    });
    zeroclaw_spawn::spawn!(async move {
        let _ = zeroclaw_gateway::run_gateway(
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
        .await;
    });
    let addr = tokio::time::timeout(WAIT, async {
        ready_rx
            .wait_for(Option::is_some)
            .await
            .expect("gateway readiness channel");
        ready_rx.borrow().expect("gateway bound address")
    })
    .await
    .expect("the in-process gateway binds");
    (addr, shutdown_tx)
}

/// One conversation on a socket: what it shows, frame by frame, with the
/// `done` frame reduced to what both gateways report.
async fn conversation(address: &str, session_id: &str) -> Vec<Value> {
    let mut chat = open_chat(address, session_id).await;
    let mut frames = vec![next_frame(&mut chat).await];
    send(&mut chat, json!({"type": "connect"})).await;
    frames.push(next_frame(&mut chat).await);
    send(
        &mut chat,
        json!({"type": "message", "content": "Say hello."}),
    )
    .await;
    frames.extend(frames_until(&mut chat, &["done", "error"]).await);
    send(&mut chat, json!({"type": "nonsense"})).await;
    frames.push(next_frame(&mut chat).await);
    send(&mut chat, json!({"type": "message", "content": ""})).await;
    frames.push(next_frame(&mut chat).await);
    send(&mut chat, json!({"type": "approval_response"})).await;
    frames.push(next_frame(&mut chat).await);
    for frame in &mut frames {
        if frame["type"] == "done" {
            *frame = json!({"type": "done", "full_response": frame["full_response"]});
        }
    }
    frames
}

#[tokio::test]
async fn the_separate_gateway_shows_what_the_in_process_gateway_shows() {
    let reply = "Hello from both gateways.";
    let in_process_provider = ScriptedProvider::spawn(vec![Reply::Text(reply)]).await;
    let core_provider = ScriptedProvider::spawn(vec![Reply::Text(reply)]).await;
    let in_process_root = tempfile::tempdir().unwrap();
    let core_root = tempfile::tempdir().unwrap();

    let (addr, shutdown) = in_process_gateway(chat_config(
        in_process_root.path(),
        &in_process_provider.base_url(),
        false,
    ))
    .await;
    let core = Core::serve(chat_config(
        core_root.path(),
        &core_provider.base_url(),
        false,
    ))
    .await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let in_process = conversation(&addr.to_string(), "parity").await;
    let separate = conversation(&gateway.address, "parity").await;
    assert_eq!(separate, in_process);
    let _ = shutdown.send(true);
}

#[tokio::test]
async fn a_failed_turn_shows_the_same_error_on_both_gateways() {
    let in_process_provider = ScriptedProvider::spawn(vec![Reply::Status(500)]).await;
    let core_provider = ScriptedProvider::spawn(vec![Reply::Status(500)]).await;
    let in_process_root = tempfile::tempdir().unwrap();
    let core_root = tempfile::tempdir().unwrap();

    let (addr, shutdown) = in_process_gateway(chat_config(
        in_process_root.path(),
        &in_process_provider.base_url(),
        false,
    ))
    .await;
    let core = Core::serve(chat_config(
        core_root.path(),
        &core_provider.base_url(),
        false,
    ))
    .await;
    let gateway = Gateway::spawn(&core.endpoint).await;

    let mut failures = Vec::new();
    for address in [addr.to_string(), gateway.address.clone()] {
        let mut chat = open_chat(&address, "failing").await;
        next_frame(&mut chat).await;
        send(
            &mut chat,
            json!({"type": "message", "content": "Say hello."}),
        )
        .await;
        let frames = frames_until(&mut chat, &["done", "error"]).await;
        failures.push(frames.last().cloned().unwrap());
    }
    assert_eq!(failures[0]["type"], "error", "{failures:?}");
    assert_eq!(failures[1], failures[0]);
    let _ = shutdown.send(true);
}

// Independent local review checks: only synthetic provider and identities.
#[tokio::test]
async fn chat_disconnect_with_approval_already_visible_matches_inprocess() {
    let mut outcomes = Vec::new();
    for separate in [false, true] {
        let provider = ScriptedProvider::spawn(vec![
            Reply::ToolCall("file_read", r#"{"path":"notes.txt"}"#),
            Reply::Text("continued after unattended approval"),
        ])
        .await;
        let tmp = tempfile::tempdir().unwrap();
        let config = chat_config(tmp.path(), &provider.base_url(), true);
        let (address, _core, _gateway, shutdown) = if separate {
            let core = Core::serve(config).await;
            let gateway = Gateway::spawn(&core.endpoint).await;
            (gateway.address.clone(), Some(core), Some(gateway), None)
        } else {
            let (addr, shutdown) = in_process_gateway(config).await;
            (addr.to_string(), None, None, Some(shutdown))
        };
        let mut socket = open_chat(&address, "parked-approval").await;
        next_frame(&mut socket).await;
        send(
            &mut socket,
            json!({"type":"message","content":"read notes"}),
        )
        .await;
        let frames = frames_until(&mut socket, &["approval_request", "error", "done"]).await;
        assert_eq!(
            frames.last().unwrap()["type"],
            "approval_request",
            "{frames:?}"
        );
        socket.close(None).await.unwrap();
        drop(socket);
        let continued = tokio::time::timeout(Duration::from_secs(4), async {
            while provider.requests.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        outcomes.push(continued);
        if let Some(tx) = shutdown {
            let _ = tx.send(true);
        }
    }
    assert_eq!(
        outcomes,
        [true, true],
        "both paths must deny already-visible unattended approvals promptly"
    );
}

#[tokio::test]
async fn chat_abort_accepts_the_key_advertised_by_its_own_list() {
    let provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut socket = open_chat(&gateway.address, "listed-turn").await;
    next_frame(&mut socket).await;
    send(&mut socket, json!({"type":"message","content":"count"})).await;
    assert_eq!(next_frame(&mut socket).await["type"], "chunk");
    let (status, body) = http(&gateway.address, "GET", "/api/sessions", Some(TOKEN)).await;
    assert_eq!(status, 200);
    let row = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_key"] == "rpc_listed-turn")
        .unwrap();
    let key = row["session_key"].as_str().unwrap();
    let result = http(
        &gateway.address,
        "POST",
        &format!("/api/sessions/{key}/abort"),
        Some(TOKEN),
    )
    .await;
    let raw_control = http(
        &gateway.address,
        "POST",
        "/api/sessions/listed-turn/abort",
        Some(TOKEN),
    )
    .await;
    assert_eq!(raw_control, (200, json!({"status":"no_active_response"})));
    assert_eq!(
        result,
        (200, json!({"status":"aborted"})),
        "the API-advertised key must address its running turn"
    );
}

#[tokio::test]
async fn chat_credential_revoked_while_waiting_for_agent_must_not_start_provider() {
    let provider =
        ScriptedProvider::spawn(vec![Reply::Text("must not start after revocation")]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut socket = open_chat(&gateway.address, "revoked-at-lock").await;
    next_frame(&mut socket).await;
    let agent = core
        .ctx
        .sessions
        .get_agent("revoked-at-lock")
        .await
        .unwrap();
    let held = agent.lock().await;
    send(&mut socket, json!({"type":"message","content":"hello"})).await;
    tokio::time::timeout(WAIT, async {
        while core
            .ctx
            .sessions
            .session_queue
            .queue_depth("revoked-at-lock")
            .await
            == 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("prompt holds admission while agent is locked");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
    assert!(core.ctx.auth.pairing().revoke_token(TOKEN));
    drop(held);
    let frames = frames_until(&mut socket, &["done", "error"]).await;
    assert_eq!(frames.last().unwrap()["type"], "error");
    let count = provider.requests.load(Ordering::SeqCst);
    let no_new_access = http(&gateway.address, "GET", "/api/health", Some(TOKEN))
        .await
        .0;
    assert_eq!(no_new_access, 401, "revocation must be real");
    assert_eq!(
        count, 0,
        "revoked queued prompt reached provider after final wait"
    );
}

#[tokio::test]
async fn chat_revocation_during_session_queue_wait_is_refused() {
    let provider = ScriptedProvider::spawn(vec![Reply::Text("must not start")]).await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut socket = open_chat(&gateway.address, "revoked-in-queue").await;
    next_frame(&mut socket).await;
    let held = core
        .ctx
        .sessions
        .session_queue
        .acquire("revoked-in-queue")
        .await
        .unwrap();
    send(&mut socket, json!({"type":"message","content":"hello"})).await;
    tokio::time::timeout(WAIT, async {
        while core
            .ctx
            .sessions
            .session_queue
            .queue_depth("revoked-in-queue")
            .await
            < 2
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(core.ctx.auth.pairing().revoke_token(TOKEN));
    drop(held);
    let frames = frames_until(&mut socket, &["done", "error"]).await;
    assert_eq!(frames.last().unwrap()["type"], "error");
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn chat_distinct_sessions_on_one_credential_keep_their_streams_separate() {
    let provider = ScriptedProvider::spawn(vec![
        Reply::Slow("alpha first second third"),
        Reply::Text("beta only"),
    ])
    .await;
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(tmp.path(), &provider.base_url(), false)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut a = open_chat(&gateway.address, "isolated-a").await;
    next_frame(&mut a).await;
    let mut b = open_chat(&gateway.address, "isolated-b").await;
    next_frame(&mut b).await;
    send(&mut a, json!({"type":"message","content":"alpha"})).await;
    let first = next_frame(&mut a).await;
    assert_eq!(first["type"], "chunk");
    send(&mut b, json!({"type":"message","content":"beta"})).await;
    let (af, bf) = tokio::join!(
        frames_until(&mut a, &["done", "error"]),
        frames_until(&mut b, &["done", "error"])
    );
    assert_eq!(
        af.last().unwrap()["full_response"],
        "alpha first second third"
    );
    assert_eq!(bf.last().unwrap()["full_response"], "beta only");
    assert_eq!(streamed_text(&bf), "beta only");
    assert!(!streamed_text(&af).contains("beta"));
}

async fn chat_http_body(address: &str, path: &str, chunked: bool) -> u16 {
    let body = "x".repeat(70_000);
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let framing = if chunked {
        "Transfer-Encoding: chunked\r\n".to_owned()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {TOKEN}\r\n{framing}Connection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes()).await.unwrap();
    if chunked {
        let encoded = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
        let _ = stream.write_all(encoded.as_bytes()).await;
    }
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let mut bytes = [0u8; 1024];
            match stream.read(&mut bytes).await {
                Ok(0) => break,
                Ok(count) => response.extend_from_slice(&bytes[..count]),
                Err(error)
                    if error.kind() == std::io::ErrorKind::ConnectionReset
                        && !response.is_empty() =>
                {
                    break;
                }
                Err(error) => panic!("no response headers: {error}"),
            }
            if response.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
    })
    .await
    .unwrap();
    String::from_utf8_lossy(&response)
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn chat_abort_content_length_and_chunked_effect_boundaries() {
    let mut results = Vec::new();
    for chunked in [false, true] {
        let provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
        let root = tempfile::tempdir().unwrap();
        let (address, shutdown) =
            in_process_gateway(chat_config(root.path(), &provider.base_url(), false)).await;
        let core_provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
        let core_root = tempfile::tempdir().unwrap();
        let core = Core::serve(chat_config(
            core_root.path(),
            &core_provider.base_url(),
            false,
        ))
        .await;
        let gateway = Gateway::spawn(&core.endpoint).await;
        for (kind, address) in [
            ("in_process", address.to_string()),
            ("separate", gateway.address.clone()),
        ] {
            let session = if chunked {
                "body-chunked"
            } else {
                "body-length"
            };
            let mut socket = open_chat(&address, session).await;
            next_frame(&mut socket).await;
            send(
                &mut socket,
                json!({"type":"message","content":"count slowly"}),
            )
            .await;
            assert_eq!(next_frame(&mut socket).await["type"], "chunk");
            let path = format!("/api/sessions/{session}/abort");
            let status = chat_http_body(&address, &path, chunked).await;
            let (_, afterwards) = http(&address, "POST", &path, Some(TOKEN)).await;
            results.push((
                kind,
                chunked,
                status,
                afterwards["status"].as_str().unwrap_or_default().to_owned(),
            ));
        }
        let _ = shutdown.send(true);
    }
    // The declared-size parity boundary must reject before cancellation.
    let expected = vec![
        ("in_process", false, 413, "aborted".to_owned()),
        ("separate", false, 413, "aborted".to_owned()),
    ];
    assert_eq!(
        results[..2],
        expected,
        "declared-size rejection must precede cancellation"
    );
    assert_eq!(
        results[2..],
        [
            ("in_process", true, 413, "aborted".to_owned()),
            ("separate", true, 413, "aborted".to_owned()),
        ],
        "chunked rejection must also precede cancellation"
    );
}

#[tokio::test]
async fn chat_stream_delivery_rechecks_revocation_at_effect() {
    let release = Arc::new(tokio::sync::Notify::new());
    let producer_release = release.clone();
    let app = Router::new().route("/v1/chat/completions", post(move |axum::Json(_request): axum::Json<Value>| {
        let release = producer_release.clone();
        async move {
            let stream = futures_util::stream::unfold((0u8, release), |(step, release)| async move {
                let body = match step {
                    0 => format!("data: {}\n\n", json!({"id":"fixture-gated", "model":"fixture-model", "choices":[{"index":0,"delta":{"role":"assistant","content":"before "}}]})),
                    1 => {
                        release.notified().await;
                        format!("data: {}\n\n", json!({"id":"fixture-gated", "model":"fixture-model", "choices":[{"index":0,"delta":{"content":"after-revoke-marker"}}]}))
                    },
                    2 => format!("data: {}\n\ndata: [DONE]\n\n", json!({"id":"fixture-gated", "model":"fixture-model", "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})),
                    _ => return None,
                };
                Some((Ok::<_,std::convert::Infallible>(body), (step+1,release)))
            });
            ([(header::CONTENT_TYPE,"text/event-stream")], Body::from_stream(stream)).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_addr = listener.local_addr().unwrap();
    let provider_task = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(
        root.path(),
        &format!("http://{provider_addr}/v1"),
        false,
    ))
    .await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut socket = open_chat(&gateway.address, "revoke-during-stream").await;
    next_frame(&mut socket).await;
    send(
        &mut socket,
        json!({"type":"message","content":"gated stream"}),
    )
    .await;
    let first = frames_until(&mut socket, &["chunk", "error", "done"]).await;
    assert_eq!(streamed_text(&first), "before ");
    assert!(core.ctx.auth.pairing().revoke_token(TOKEN));
    // No later provider byte can predate the completed revocation.
    release.notify_one();
    let frames = frames_until(&mut socket, &["done", "error"]).await;
    let leaked = frames
        .iter()
        .any(|frame| frame.to_string().contains("after-revoke-marker"));
    provider_task.abort();
    assert!(
        !leaked,
        "new stream bytes must not reach a credential revoked before those bytes existed"
    );
}

#[tokio::test]
async fn chat_stalled_abort_body_times_out_before_cancellation_on_both_gateways() {
    for separate in [false, true] {
        let provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
        let root = tempfile::tempdir().unwrap();
        let mut config = chat_config(root.path(), &provider.base_url(), false);
        config.gateway.request_timeout_secs = 2;
        let (address, _core, _gateway, shutdown) = if separate {
            let core = Core::serve(config).await;
            let gateway =
                Gateway::spawn_with_args(&core.endpoint, &["--request-timeout", "2"]).await;
            (gateway.address.clone(), Some(core), Some(gateway), None)
        } else {
            let (addr, shutdown) = in_process_gateway(config).await;
            (addr.to_string(), None, None, Some(shutdown))
        };
        let mut socket = open_chat(&address, "stalled-abort").await;
        next_frame(&mut socket).await;
        send(
            &mut socket,
            json!({"type":"message","content":"count slowly"}),
        )
        .await;
        assert_eq!(next_frame(&mut socket).await["type"], "chunk");
        let mut stream = tokio::net::TcpStream::connect(&address).await.unwrap();
        let header = format!(
            "POST /api/sessions/stalled-abort/abort HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Length: 1\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(header.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut bytes = [0; 1024];
                let n = stream.read(&mut bytes).await.unwrap();
                assert!(n > 0, "a stalled body must receive timeout headers");
                response.extend_from_slice(&bytes[..n]);
                if response.windows(4).any(|p| p == b"\r\n\r\n") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let status: u16 = String::from_utf8_lossy(&response)
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(status, 408, "separate={separate}");
        assert_eq!(
            http(
                &address,
                "POST",
                "/api/sessions/stalled-abort/abort",
                Some(TOKEN)
            )
            .await,
            (200, json!({"status":"aborted"})),
            "timeout must have no cancellation effect"
        );
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(true);
        }
    }
}

#[tokio::test]
async fn chat_same_version_legacy_session_new_is_skew_refused() {
    use std::os::unix::fs::PermissionsExt;
    let provider = ScriptedProvider::spawn(vec![Reply::Text("unused")]).await;
    let root = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(root.path(), &provider.base_url(), false)).await;
    let direct = Gateway::spawn(&core.endpoint).await;
    let refused = |address: String| async move {
        let uri =
            format!("ws://{address}/ws/chat?agent=nobody&session_id=legacy-check&token={TOKEN}");
        match tokio_tungstenite::connect_async(uri).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                let text = response
                    .body()
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                    .unwrap_or_default();
                (response.status(), text)
            }
            other => panic!("an unknown agent cannot upgrade: {other:?}"),
        }
    };
    let current = refused(direct.address.clone()).await;
    assert_eq!(current.0, StatusCode::BAD_REQUEST);
    let proxy_dir = tempfile::tempdir().unwrap();
    let endpoint = proxy_dir.path().join("core.sock");
    let listener = tokio::net::UnixListener::bind(&endpoint).unwrap();
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = core.endpoint.clone();
    let rewrites = Arc::new(AtomicUsize::new(0));
    let count = rewrites.clone();
    let handshakes = Arc::new(AtomicUsize::new(0));
    let seen_handshakes = handshakes.clone();
    // Forward the real core unchanged, except for its pre-PR error contract:
    // the unknown-agent SessionNew error is -32603 rather than -32602.
    // Credentials and protocol/package version are untouched. The feature set is the older implementation's; without the gate its unknown-agent response is still the legacy error.
    let proxy = zeroclaw_spawn::spawn!(async move {
        loop {
            let (front, _) = listener.accept().await.unwrap();
            let back = tokio::net::UnixStream::connect(&backend).await.unwrap();
            let count = count.clone();
            let seen_handshakes = seen_handshakes.clone();
            zeroclaw_spawn::spawn!(async move {
                let (front_read, mut front_write) = front.into_split();
                let (back_read, mut back_write) = back.into_split();
                let old_ids = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
                let upstream_ids = old_ids.clone();
                let upstream = async move {
                    let mut lines = BufReader::new(front_read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let frame: Value = serde_json::from_str(&line).unwrap();
                        if frame["method"] == "session/new"
                            && frame["params"]["agent_alias"] == "nobody"
                        {
                            upstream_ids.lock().unwrap().insert(frame["id"].to_string());
                        }
                        if back_write
                            .write_all(format!("{line}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                };
                let downstream = async move {
                    let mut lines = BufReader::new(back_read).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let mut frame: Value = serde_json::from_str(&line).unwrap();
                        if frame["result"]["server_version"].as_str()
                            == Some(env!("CARGO_PKG_VERSION"))
                        {
                            frame["result"]["features"] =
                                json!([zeroclaw_rpc_proto::feature::TUI_CLIENT_KIND]);
                            seen_handshakes.fetch_add(1, Ordering::SeqCst);
                        }
                        let older_error = old_ids.lock().unwrap().remove(&frame["id"].to_string());
                        if older_error && frame["error"]["code"] == -32602 {
                            frame["error"]["code"] = json!(-32603);
                            frame["error"]["message"] =
                                json!("Failed to create agent: unknown agent nobody");
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                        if front_write
                            .write_all(format!("{frame}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                };
                tokio::join!(upstream, downstream);
            });
        }
    });
    let legacy_client = zeroclaw_rpc_client::RpcClient::connect_local(
        &endpoint,
        zeroclaw_rpc_client::ConnectOptions {
            auth_token: Some(TOKEN.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        legacy_client.handshake().server_version,
        env!("CARGO_PKG_VERSION")
    );
    let legacy_error = legacy_client
        .request(
            zeroclaw_rpc_client::Method::SessionNew,
            json!({"agent_alias":"nobody", "session_id":"legacy-direct-control"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(legacy_error, zeroclaw_rpc_client::ClientError::Rpc(ref error) if error.code == -32603)
    );
    assert_eq!(
        rewrites.load(Ordering::SeqCst),
        1,
        "the emulated older behavior is real"
    );
    let gateway = Gateway::spawn(&endpoint).await;
    let legacy = refused(gateway.address.clone()).await;
    assert_eq!(
        rewrites.load(Ordering::SeqCst),
        1,
        "the gateway must make no SessionNew call after the direct legacy control"
    );
    assert!(
        handshakes.load(Ordering::SeqCst) > 0,
        "the older implementation reports the same package version"
    );
    assert!(legacy.1.contains("core_capability_missing"), "{legacy:?}");
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
    proxy.abort();
    assert_eq!(
        legacy.0,
        StatusCode::SERVICE_UNAVAILABLE,
        "a peer missing required chat semantics must be refused as incompatible, not accepted because its package/protocol version matches"
    );
}

#[tokio::test]
async fn chat_listed_keys_and_prefix_shaped_raw_ids_never_cancel_the_other_turn() {
    for raw_second in [false, true] {
        let provider = ScriptedProvider::spawn(vec![Reply::Slow(LONG_REPLY)]).await;
        let root = tempfile::tempdir().unwrap();
        let core = Core::serve(chat_config(root.path(), &provider.base_url(), false)).await;
        let gateway = Gateway::spawn(&core.endpoint).await;
        let mut a = open_chat(&gateway.address, "target").await;
        next_frame(&mut a).await;
        let mut b = open_chat(&gateway.address, "rpc_target").await;
        next_frame(&mut b).await;
        send(&mut a, json!({"type":"message","content":"first"})).await;
        assert_eq!(next_frame(&mut a).await["type"], "chunk");
        send(&mut b, json!({"type":"message","content":"second"})).await;
        assert_eq!(next_frame(&mut b).await["type"], "chunk");
        let (status, list) = http(&gateway.address, "GET", "/api/sessions", Some(TOKEN)).await;
        assert_eq!(status, 200);
        for key in ["rpc_target", "rpc_rpc_target"] {
            assert!(
                list["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["session_key"] == key)
            );
        }
        assert_eq!(
            http(
                &gateway.address,
                "POST",
                "/api/sessions/rpc_target/abort",
                Some(TOKEN)
            )
            .await,
            (200, json!({"status":"aborted"}))
        );
        assert_eq!(
            frames_until(&mut a, &["aborted", "done", "error"])
                .await
                .last()
                .unwrap()["type"],
            "aborted"
        );
        assert!(
            core.ctx.sessions.has_inflight_turn("rpc_target"),
            "the other raw id remains active"
        );
        let second = if raw_second {
            "/api/sessions/rpc_target/abort?address=raw"
        } else {
            "/api/sessions/rpc_rpc_target/abort"
        };
        assert_eq!(
            http(&gateway.address, "POST", second, Some(TOKEN)).await,
            (200, json!({"status":"aborted"}))
        );
        assert_eq!(
            frames_until(&mut b, &["aborted", "done", "error"])
                .await
                .last()
                .unwrap()["type"],
            "aborted"
        );
    }
}

#[tokio::test]
async fn chat_disconnect_keeps_an_unrelated_viewers_approval_pending() {
    let provider = ScriptedProvider::spawn(vec![
        Reply::ToolCall("file_read", r#"{"path":"notes.txt"}"#),
        Reply::ToolCall("file_read", r#"{"path":"notes.txt"}"#),
        Reply::Text("continued"),
    ])
    .await;
    let root = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(root.path(), &provider.base_url(), true)).await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut a = open_chat(&gateway.address, "viewer-a").await;
    next_frame(&mut a).await;
    let mut b = open_chat(&gateway.address, "viewer-b").await;
    next_frame(&mut b).await;
    send(&mut a, json!({"type":"message","content":"read first"})).await;
    let first = frames_until(&mut a, &["approval_request", "done", "error"]).await;
    let first_id = first.last().unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&mut b, json!({"type":"message","content":"read second"})).await;
    let second = frames_until(&mut b, &["approval_request", "done", "error"]).await;
    let second_id = second.last().unwrap()["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(first_id, second_id);
    a.close(None).await.unwrap();
    drop(a);
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.ctx.approval_pending.session_for(&first_id).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("this viewer's parked responder is closed promptly");
    assert_eq!(
        core.ctx.approval_pending.session_for(&second_id).as_deref(),
        Some("viewer-b")
    );
    send(
        &mut b,
        json!({"type":"approval_response", "request_id":second_id, "decision":"approve"}),
    )
    .await;
    assert_eq!(
        frames_until(&mut b, &["done", "error"])
            .await
            .last()
            .unwrap()["type"],
        "done"
    );
}

#[tokio::test]
async fn chat_an_approval_raised_after_disconnect_is_unreachable_without_parking() {
    let release = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(AtomicUsize::new(0));
    let producer_release = Arc::clone(&release);
    let count = Arc::clone(&requests);
    let app = Router::new().route("/v1/chat/completions", post(move |axum::Json(request): axum::Json<Value>| {
        let release = Arc::clone(&producer_release);
        let count = Arc::clone(&count);
        async move {
            if count.fetch_add(1, Ordering::SeqCst) > 0 {
                return provider_response(Reply::Text("continued after unreachable approval"),
                    request["stream"].as_bool().unwrap_or(false));
            }
            let stream = futures_util::stream::unfold((0u8, release), |(step, release)| async move {
                let packet = match step {
                    0 => json!({"id":"approval-after-detach", "model":"fixture-model", "choices":[{"index":0,"delta":{"role":"assistant","content":"before "}}]}),
                    1 => {
                        release.notified().await;
                        json!({"id":"approval-after-detach", "model":"fixture-model", "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"later-tool","type":"function","function":{"name":"file_read","arguments":"{\"path\":\"notes.txt\"}"}}]}}]})
                    }
                    2 => json!({"id":"approval-after-detach", "model":"fixture-model", "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                    _ => return None,
                };
                let ending = if step == 2 { "data: [DONE]\n\n" } else { "" };
                Some((Ok::<_, std::convert::Infallible>(format!("data: {packet}\n\n{ending}")), (step + 1, release)))
            });
            ([(header::CONTENT_TYPE, "text/event-stream")], Body::from_stream(stream)).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let core = Core::serve(chat_config(
        root.path(),
        &format!("http://{address}/v1"),
        true,
    ))
    .await;
    let gateway = Gateway::spawn(&core.endpoint).await;
    let mut socket = open_chat(&gateway.address, "approval-after-detach").await;
    next_frame(&mut socket).await;
    send(
        &mut socket,
        json!({"type":"message", "content":"read after the barrier"}),
    )
    .await;
    assert_eq!(next_frame(&mut socket).await["type"], "chunk");
    socket.close(None).await.unwrap();
    drop(socket);
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while requests.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("future unattended approval must not wait for the ordinary timeout");
    provider.abort();
}
