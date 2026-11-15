use super::*;
use axum::response::IntoResponse;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::ThreadId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroclaw_api::bridge_intake::{BridgeInput, BridgeReceipt, BridgeResume, BridgeSource};
use zeroclaw_config::schema::{
    AliasedAgentConfig, CustomModelProviderConfig, ModelProviderConfig, RiskProfileConfig,
    RuntimeProfileConfig,
};
use zeroclaw_infra::session_backend::{
    RequestReceipt, SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
    TimestampedMessage,
};
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;
use zeroclaw_providers::ChatMessage;

struct AliasPauseGate {
    target_call: usize,
    entered_tx: tokio::sync::mpsc::UnboundedSender<()>,
    delete_probed_tx: tokio::sync::mpsc::UnboundedSender<()>,
    stamp_finished_tx: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

struct CountingSessionBackend {
    inner: Arc<dyn SessionBackend>,
    runtime_thread: ThreadId,
    load_calls: AtomicUsize,
    metadata_calls: AtomicUsize,
    alias_calls: AtomicUsize,
    name_calls: AtomicUsize,
    async_thread_violations: AtomicUsize,
    alias_pause: Option<AliasPauseGate>,
}

impl CountingSessionBackend {
    fn new(inner: Arc<dyn SessionBackend>, runtime_thread: ThreadId) -> Self {
        Self {
            inner,
            runtime_thread,
            load_calls: AtomicUsize::new(0),
            metadata_calls: AtomicUsize::new(0),
            alias_calls: AtomicUsize::new(0),
            name_calls: AtomicUsize::new(0),
            async_thread_violations: AtomicUsize::new(0),
            alias_pause: None,
        }
    }

    fn with_alias_pause(
        inner: Arc<dyn SessionBackend>,
        runtime_thread: ThreadId,
        alias_pause: AliasPauseGate,
    ) -> Self {
        let mut counting = Self::new(inner, runtime_thread);
        counting.alias_pause = Some(alias_pause);
        counting
    }

    fn record_blocking_check(&self) {
        if std::thread::current().id() == self.runtime_thread {
            self.async_thread_violations.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn reset(&self) {
        self.load_calls.store(0, Ordering::SeqCst);
        self.metadata_calls.store(0, Ordering::SeqCst);
        self.async_thread_violations.store(0, Ordering::SeqCst);
    }

    fn counts(&self) -> (usize, usize, usize) {
        (
            self.load_calls.load(Ordering::SeqCst),
            self.metadata_calls.load(Ordering::SeqCst),
            self.async_thread_violations.load(Ordering::SeqCst),
        )
    }
}

impl SessionBackend for CountingSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        self.record_blocking_check();
        self.load_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.load(session_key)
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        self.inner.load_with_timestamps(session_key)
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.inner.append(session_key, message)
    }

    fn append_with_ingress(
        &self,
        session_key: &str,
        message: &ChatMessage,
        ingress: &zeroclaw_api::review::UserMessageIngress,
    ) -> std::io::Result<()> {
        self.inner
            .append_with_ingress(session_key, message, ingress)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.inner.remove_last(session_key)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.inner.update_last(session_key, message)
    }

    fn list_sessions(&self) -> Vec<String> {
        self.inner.list_sessions()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        self.inner.list_sessions_with_metadata()
    }

    fn compact(&self, session_key: &str) -> std::io::Result<()> {
        self.inner.compact(session_key)
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        self.inner.cleanup_stale(ttl_hours)
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        self.inner.search(query)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        self.inner.clear_messages(session_key)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        self.inner.delete_session(session_key)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        self.inner.clear_agent_attribution(agent_alias)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        self.inner.rename_agent_attribution(from, to)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        self.inner.count_agent_attribution(agent_alias)
    }

    fn session_exists(&self, session_key: &str) -> bool {
        if let Some(gate) = &self.alias_pause {
            let _ = gate.delete_probed_tx.send(());
        }
        self.inner.session_exists(session_key)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        self.record_blocking_check();
        let name_call = self.name_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let res = self.inner.set_session_name(session_key, name);
        if let Some(gate) = &self.alias_pause
            && name_call == gate.target_call
        {
            let _ = gate.stamp_finished_tx.send(());
        }
        res
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        self.record_blocking_check();
        self.inner.get_session_name(session_key)
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        self.record_blocking_check();
        let call = self.alias_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(gate) = &self.alias_pause
            && call == gate.target_call
        {
            let _ = gate.entered_tx.send(());
            let (lock, cvar) = &*gate.release;
            let guard = lock.lock().unwrap_or_else(|p| p.into_inner());
            let (_guard, timeout_res) = cvar
                .wait_timeout_while(guard, Duration::from_secs(5), |released| !*released)
                .unwrap_or_else(|p| p.into_inner());
            assert!(
                !timeout_res.timed_out(),
                "AliasPauseGate timed out waiting for test release (call #{call})"
            );
        }
        self.inner.set_session_agent_alias(session_key, agent_alias)
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        self.record_blocking_check();
        self.inner.get_session_agent_alias(session_key)
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        self.record_blocking_check();
        self.inner.set_session_context(session_key, context)
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        self.record_blocking_check();
        self.metadata_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get_session_metadata(session_key)
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        self.inner.set_session_state(session_key, state, turn_id)
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        self.inner.get_session_state(session_key)
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        self.inner.list_running_sessions()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        self.inner.list_stuck_sessions(threshold_secs)
    }

    fn record_request(
        &self,
        session_key: &str,
        request_id: &str,
        state: &str,
    ) -> std::io::Result<Option<RequestReceipt>> {
        self.inner.record_request(session_key, request_id, state)
    }

    fn set_request_state(
        &self,
        session_key: &str,
        request_id: &str,
        state: &str,
    ) -> std::io::Result<()> {
        self.inner.set_request_state(session_key, request_id, state)
    }

    fn bridge_resume(&self, source: &BridgeSource) -> std::io::Result<BridgeResume> {
        self.inner.bridge_resume(source)
    }

    fn bridge_receipt(
        &self,
        source: &BridgeSource,
        update_id: i64,
    ) -> std::io::Result<Option<BridgeReceipt>> {
        self.inner.bridge_receipt(source, update_id)
    }

    fn bridge_record(
        &self,
        source: &BridgeSource,
        input: &BridgeInput,
    ) -> std::io::Result<BridgeReceipt> {
        self.inner.bridge_record(source, input)
    }

    fn bridge_claim(&self, source: &BridgeSource, update_id: i64) -> std::io::Result<bool> {
        self.inner.bridge_claim(source, update_id)
    }

    fn bridge_finish(
        &self,
        source: &BridgeSource,
        update_id: i64,
        state: &str,
    ) -> std::io::Result<()> {
        self.inner.bridge_finish(source, update_id, state)
    }
}

const LAZY_OWNER_TOKEN: &str = "lazy-history-owner-token";

struct RawWsClient {
    stream: tokio::net::TcpStream,
    buf: Vec<u8>,
}

impl RawWsClient {
    async fn connect(address: std::net::SocketAddr, path_and_query: &str) -> Self {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET {path_and_query} HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {WS_PROTOCOL}\r\nAuthorization: Bearer {LAZY_OWNER_TOKEN}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut raw = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0, "EOF before HTTP 101 headers completed");
            raw.extend_from_slice(&chunk[..n]);
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let status_line = std::str::from_utf8(&raw[..header_end])
            .unwrap()
            .lines()
            .next()
            .unwrap();
        assert!(
            status_line.contains("101"),
            "expected HTTP 101 Switching Protocols, got: {status_line}"
        );
        let overflow = raw.split_off(header_end);
        Self {
            stream,
            buf: overflow,
        }
    }

    async fn read_exact_buffered(&mut self, len: usize) -> Vec<u8> {
        let mut tmp = [0_u8; 1024];
        while self.buf.len() < len {
            let n = tokio::time::timeout(Duration::from_secs(5), self.stream.read(&mut tmp))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0, "unexpected WebSocket EOF");
            self.buf.extend_from_slice(&tmp[..n]);
        }
        let rest = self.buf.split_off(len);
        std::mem::replace(&mut self.buf, rest)
    }

    async fn read_json(&mut self) -> Value {
        loop {
            let head = self.read_exact_buffered(2).await;
            let opcode = head[0] & 0x0f;
            assert_eq!(head[1] & 0x80, 0, "unexpected masked server frame");
            let payload_len = match head[1] & 0x7f {
                126 => {
                    let ext = self.read_exact_buffered(2).await;
                    u16::from_be_bytes([ext[0], ext[1]]) as usize
                }
                127 => {
                    let ext = self.read_exact_buffered(8).await;
                    u64::from_be_bytes(ext.try_into().unwrap()) as usize
                }
                n => n as usize,
            };
            let payload = self.read_exact_buffered(payload_len).await;
            match opcode {
                0x1 => return serde_json::from_slice(&payload).unwrap(),
                0x8 => panic!("unexpected WebSocket close frame: {:?}", payload),
                0x9 => self.send_frame(0xA, &payload).await,
                0xA => continue,
                other => panic!("unexpected WebSocket opcode {other:#x}"),
            }
        }
    }

    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) {
        let mask = [0x12_u8, 0x34, 0x56, 0x78];
        let mut frame = vec![0x80 | opcode];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i % 4]);
        }
        self.stream.write_all(&frame).await.unwrap();
    }

    async fn send_json(&mut self, value: Value) {
        let bytes = serde_json::to_vec(&value).unwrap();
        self.send_frame(0x1, &bytes).await;
    }

    async fn until(&mut self, step: &str, frame_type: &str) -> Value {
        let mut seen_frames = Vec::new();
        let wait = async {
            loop {
                let frame = self.read_json().await;
                if frame["type"] == frame_type {
                    return frame;
                }
                if matches!(
                    frame["type"].as_str(),
                    Some("error" | "turn_error" | "turn_gap")
                ) {
                    panic!(
                        "unexpected error frame during `{step}` waiting for `{frame_type}`: {frame}"
                    );
                }
                seen_frames.push(frame);
            }
        };
        match tokio::time::timeout(Duration::from_secs(5), wait).await {
            Ok(frame) => frame,
            Err(_) => panic!(
                "timed out during `{step}` waiting for `{frame_type}`; seen frames: {seen_frames:?}"
            ),
        }
    }

    async fn close(mut self) {
        self.send_frame(0x8, &[]).await;
        let _ = self.stream.shutdown().await;
    }

    async fn wait_for_eof(mut self) {
        let mut buf = [0_u8; 256];
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match self.stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        })
        .await
        .expect("timed out waiting for WebSocket EOF after gateway shutdown");
    }
}

async fn mock_lazy_provider(
    axum::extract::State(seen): axum::extract::State<Arc<parking_lot::Mutex<Vec<Value>>>>,
    axum::Json(request): axum::Json<Value>,
) -> impl IntoResponse {
    seen.lock().push(request.clone());
    let message = json!({"role": "assistant", "content": "lazy response"});
    if request["stream"] == true {
        (
            [("content-type", "text/event-stream")],
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"id":"local-response","choices":[{"index":0,"delta":message,"finish_reason":"stop"}]})
            ),
        )
            .into_response()
    } else {
        axum::Json(
            json!({"id":"local-response","choices":[{"index":0,"message":message,"finish_reason":"stop"}]}),
        )
        .into_response()
    }
}

async fn http_delete_session(address: std::net::SocketAddr, session_id: &str) -> u16 {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let req = format!(
        "DELETE /api/sessions/{session_id} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {LAZY_OWNER_TOKEN}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.unwrap();
    let head = std::str::from_utf8(&resp).unwrap();
    head.lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap()
}

struct LazyHarness {
    state: AppState,
    address: std::net::SocketAddr,
    seen: Arc<parking_lot::Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
    provider_task: tokio::task::JoinHandle<()>,
}

impl Drop for LazyHarness {
    fn drop(&mut self) {
        self.server.abort();
        self.provider_task.abort();
    }
}

async fn spawn_lazy_harness(
    tmp: &tempfile::TempDir,
    counting: Arc<CountingSessionBackend>,
) -> LazyHarness {
    let workspace = tmp.path().join("agent-workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let seen = Arc::new(parking_lot::Mutex::new(Vec::<Value>::new()));
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_uri = format!(
        "http://{}/v1/chat/completions",
        provider_listener.local_addr().unwrap()
    );
    let provider_app = axum::Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(mock_lazy_provider),
        )
        .with_state(Arc::clone(&seen));
    let provider_task = zeroclaw_spawn::spawn!(async move {
        axum::serve(provider_listener, provider_app).await.unwrap();
    });

    let mut state = crate::tests::admin_paircode_state(tmp, true, false);
    state.pairing = Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
        true,
        &[LAZY_OWNER_TOKEN.into()],
    ));
    {
        let mut config = state.config.write();
        config.providers.models.custom.insert(
            "test".into(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("local-lazy-test".into()),
                    uri: Some(provider_uri),
                    native_tools: Some(true),
                    timeout_secs: Some(5),
                    ..Default::default()
                },
            },
        );
        config.risk_profiles.insert(
            "test".into(),
            RiskProfileConfig {
                workspace_only: true,
                allowed_tools: Some(Vec::new()),
                ..Default::default()
            },
        );
        config.runtime_profiles.insert(
            "test".into(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 2,
                ..Default::default()
            },
        );
        let mut agent = AliasedAgentConfig {
            model_provider: "custom.test".into(),
            risk_profile: "test".into(),
            runtime_profile: "test".into(),
            ..Default::default()
        };
        agent.workspace.path = Some(workspace);
        agent.memory.backend = zeroclaw_config::multi_agent::MemoryBackendKind::None;
        config.agents.insert("web".into(), agent);
    }

    state.session_backend = Some(counting);

    let app = axum::Router::new()
        .route("/ws/chat", axum::routing::get(handle_ws_chat))
        .route(
            "/api/sessions/{id}",
            axum::routing::delete(crate::api::handle_api_session_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });

    LazyHarness {
        state,
        address,
        seen,
        server,
        provider_task,
    }
}

#[tokio::test]
async fn ws_chat_lazy_history_defers_load_to_cold_attach_on_blocking_pool_and_preserves_delete() {
    let runtime_thread = std::thread::current().id();
    let tmp = tempfile::tempdir().unwrap();

    let sqlite = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
    sqlite
        .set_session_agent_alias("gw_lazy-one", "web")
        .unwrap();
    for i in 0..6 {
        let msg = if i % 2 == 0 {
            ChatMessage::user(format!("seeded-user-msg-{i}"))
        } else {
            ChatMessage::assistant(format!("seeded-assistant-msg-{i}"))
        };
        sqlite.append("gw_lazy-one", &msg).unwrap();
    }

    let counting = Arc::new(CountingSessionBackend::new(sqlite, runtime_thread));
    let harness = spawn_lazy_harness(&tmp, Arc::clone(&counting)).await;
    let address = harness.address;
    let state = harness.state.clone();
    let seen = Arc::clone(&harness.seen);

    // 1. Opening a socket without sending `connect` reads metadata (6 messages)
    //    for `session_start` on the blocking pool and performs ZERO `load()` calls.
    let mut pre_connect =
        RawWsClient::connect(address, "/ws/chat?agent=web&session_id=lazy-one").await;
    let start_frame = pre_connect.until("pre_connect", "session_start").await;
    assert_eq!(start_frame["session_id"], "lazy-one");
    assert_eq!(start_frame["resumed"], true);
    assert_eq!(start_frame["message_count"], 6);
    assert_eq!(counting.counts(), (0, 1, 0));
    pre_connect.close().await;

    counting.reset();

    // 2. First socket completing handshake + `attach` initializes the cold slot
    //    with 1 `load()`, synchronized by awaiting `turn_ready` (post-`attach`).
    let mut client_a =
        RawWsClient::connect(address, "/ws/chat?agent=web&session_id=lazy-one").await;
    let start_a = client_a.until("client_a", "session_start").await;
    assert_eq!(start_a["resumed"], true);
    assert_eq!(start_a["message_count"], 6);
    client_a.send_json(json!({"type": "connect"})).await;
    client_a.until("client_a", "connected").await;
    client_a
        .send_json(json!({"type": "turn_control", "version": 1, "op": "negotiate"}))
        .await;
    client_a.until("client_a", "turn_ready").await;
    assert_eq!(counting.counts(), (1, 1, 0));

    // 3. Second socket attaching to the live conversation shares the existing slot
    //    with 0 additional `load()` calls, and running a turn sees all 6 seeded rows.
    let mut client_b =
        RawWsClient::connect(address, "/ws/chat?agent=web&session_id=lazy-one").await;
    let start_b = client_b.until("client_b", "session_start").await;
    assert_eq!(start_b["resumed"], true);
    assert_eq!(start_b["message_count"], 6);
    client_b.send_json(json!({"type": "connect"})).await;
    client_b.until("client_b", "connected").await;
    client_b
        .send_json(json!({"type": "turn_control", "version": 1, "op": "negotiate"}))
        .await;
    let ready_b = client_b.until("client_b", "turn_ready").await;
    assert_eq!(counting.counts(), (1, 2, 0));

    let epoch_b = ready_b["epoch"].as_str().unwrap();
    let req_b = uuid::Uuid::new_v4().to_string();
    client_b
        .send_json(json!({
            "type": "turn_control",
            "version": 1,
            "op": "message",
            "epoch": epoch_b,
            "request_id": req_b,
            "content": "check-seeded"
        }))
        .await;
    let ack_b = client_b.until("client_b_turn", "turn_ack").await;
    let turn_b = ack_b["turn_id"].as_str().unwrap();
    loop {
        let frame = client_b.read_json().await;
        if matches!(
            frame["type"].as_str(),
            Some("error" | "turn_error" | "turn_gap")
        ) {
            panic!("unexpected error frame during seeded turn: {frame}");
        }
        if frame["type"] == "turn_event" && frame["turn_id"] == turn_b && frame["terminal"] == true
        {
            break;
        }
    }
    {
        let recorded = seen.lock();
        assert_eq!(recorded.len(), 1);
        let input = recorded[0]["messages"].to_string();
        assert!(input.contains("seeded-user-msg-0"));
        assert!(input.contains("seeded-assistant-msg-5"));
        assert!(input.contains("check-seeded"));
    }

    // 4. Mid-handshake DELETE invariant: open a raw socket through `session_start`,
    //    detach all live clients, DELETE `/api/sessions/lazy-one`, then finish
    //    `connect` and verify seeded messages are NOT restored.
    let mut waiting = RawWsClient::connect(address, "/ws/chat?agent=web&session_id=lazy-one").await;
    let waiting_start = waiting.until("waiting", "session_start").await;
    assert_eq!(waiting_start["resumed"], true);
    assert_eq!(waiting_start["message_count"], 8);

    client_a.close().await;
    client_b.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.ws_conversations.len() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert_eq!(http_delete_session(address, "lazy-one").await, 200);

    waiting.send_json(json!({"type": "connect"})).await;
    waiting.until("waiting", "connected").await;
    waiting
        .send_json(
            json!({"type": "message", "id": "m-after-delete", "content": "fresh-after-delete"}),
        )
        .await;
    let done = waiting.until("waiting_after_delete", "done").await;
    assert_eq!(done["type"], "done");
    let (_, _, violations) = counting.counts();
    assert_eq!(violations, 0);

    {
        let recorded = seen.lock();
        assert_eq!(recorded.len(), 2);
        let post_delete_input = recorded[1]["messages"].to_string();
        assert!(post_delete_input.contains("fresh-after-delete"));
        assert!(!post_delete_input.contains("seeded-user-msg-0"));
        assert!(!post_delete_input.contains("check-seeded"));
    }

    waiting.close().await;
}

async fn assert_shutdown_during_stamp_holds_lifecycle_against_delete(target_alias_call: usize) {
    let runtime_thread = std::thread::current().id();
    let tmp = tempfile::tempdir().unwrap();
    let sqlite = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
    sqlite
        .set_session_agent_alias("gw_cancel-race", "web")
        .unwrap();
    sqlite
        .set_session_name("gw_cancel-race", "BeforeDelete")
        .unwrap();
    sqlite
        .append("gw_cancel-race", &ChatMessage::user("seed-cancel"))
        .unwrap();

    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (delete_probed_tx, mut delete_probed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (stamp_finished_tx, mut stamp_finished_rx) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let gate = AliasPauseGate {
        target_call: target_alias_call,
        entered_tx,
        delete_probed_tx,
        stamp_finished_tx,
        release: Arc::clone(&release),
    };
    let sqlite_dyn: Arc<dyn SessionBackend> = sqlite.clone();
    let counting = Arc::new(CountingSessionBackend::with_alias_pause(
        sqlite_dyn,
        runtime_thread,
        gate,
    ));
    let harness = spawn_lazy_harness(&tmp, Arc::clone(&counting)).await;

    let mut client = RawWsClient::connect(
        harness.address,
        "/ws/chat?agent=web&session_id=cancel-race&name=StaleName",
    )
    .await;
    if target_alias_call > 1 {
        let start = client.until("cancel_race_start", "session_start").await;
        assert_eq!(start["resumed"], true);
        client.send_json(json!({"type": "connect"})).await;
    }

    tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
        .await
        .expect("timed out waiting for blocking stamp_session to enter set_session_agent_alias")
        .expect("entered_tx closed unexpectedly");

    // Trigger gateway_shutdown so ws_handler drops `handle_socket` while `stamp_session`
    // is still paused in `spawn_blocking`. Draining client TCP to EOF confirms drop.
    let _ = harness.state.shutdown_tx.send(true);
    client.wait_for_eof().await;

    // Deterministically verify the cancelled blocking task still owns the lifecycle mutex.
    let lifecycle = harness
        .state
        .ws_conversations
        .session_lifecycle("gw_cancel-race");
    assert!(
        lifecycle.try_lock().is_err(),
        "lifecycle lock was released prematurely upon WS cancellation (call #{target_alias_call})"
    );

    // Spawn DELETE and wait until its handler passes `resolve_session_key` (`session_exists`),
    // five synchronous lines before `lifecycle.lock().await`.
    let delete_done = Arc::new(AtomicBool::new(false));
    let delete_done_flag = Arc::clone(&delete_done);
    let delete_addr = harness.address;
    let delete_task = zeroclaw_spawn::spawn!(async move {
        let status = http_delete_session(delete_addr, "cancel-race").await;
        delete_done_flag.store(true, Ordering::SeqCst);
        status
    });
    tokio::time::timeout(Duration::from_secs(5), delete_probed_rx.recv())
        .await
        .expect("timed out waiting for DELETE handler to probe session_exists")
        .expect("delete_probed_tx closed unexpectedly");
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        lifecycle.try_lock().is_err(),
        "lifecycle lock unexpectedly free while DELETE is waiting (call #{target_alias_call})"
    );
    assert!(
        !delete_done.load(Ordering::SeqCst),
        "DELETE completed before cancelled blocking stamp task released lifecycle guard (call #{target_alias_call})"
    );

    {
        let (lock, cvar) = &*release;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }

    // Independently wait for both the blocking stamp writes and DELETE completion
    // before asserting zero resurrection on SQLite.
    tokio::time::timeout(Duration::from_secs(5), stamp_finished_rx.recv())
        .await
        .expect("timed out waiting for paused stamp_session to finish writes")
        .expect("stamp_finished_tx closed unexpectedly");
    let status = tokio::time::timeout(Duration::from_secs(5), delete_task)
        .await
        .expect("DELETE task timed out after releasing blocking stamp gate")
        .unwrap();
    assert_eq!(status, 200);
    assert!(!sqlite.session_exists("gw_cancel-race"));
    assert!(sqlite.get_session_metadata("gw_cancel-race").is_none());
    assert_eq!(
        sqlite.get_session_agent_alias("gw_cancel-race").unwrap(),
        None
    );
    assert!(
        sqlite
            .get_session_name("gw_cancel-race")
            .ok()
            .flatten()
            .is_none(),
        "late stamp_session resurrected deleted session name"
    );
    assert!(sqlite.list_sessions_with_metadata().is_empty());
    assert_eq!(counting.counts().2, 0);
}

#[tokio::test]
async fn ws_chat_shutdown_during_pre_start_stamp_holds_lifecycle_against_delete() {
    assert_shutdown_during_stamp_holds_lifecycle_against_delete(1).await;
}

#[tokio::test]
async fn ws_chat_shutdown_during_attach_stamp_holds_lifecycle_against_delete() {
    assert_shutdown_during_stamp_holds_lifecycle_against_delete(2).await;
}

#[tokio::test]
async fn ws_chat_jsonl_backend_runs_on_blocking_pool_and_skips_warm_attach_load() {
    let runtime_thread = std::thread::current().id();
    let tmp = tempfile::tempdir().unwrap();
    let jsonl = zeroclaw_infra::make_session_backend(tmp.path(), "jsonl").unwrap();
    jsonl
        .append("gw_lazy-jsonl", &ChatMessage::user("seeded-jsonl-0"))
        .unwrap();
    jsonl
        .append(
            "gw_lazy-jsonl",
            &ChatMessage::assistant("seeded-jsonl-1"),
        )
        .unwrap();

    let counting = Arc::new(CountingSessionBackend::new(jsonl, runtime_thread));
    let harness = spawn_lazy_harness(&tmp, Arc::clone(&counting)).await;

    let mut pre_connect =
        RawWsClient::connect(harness.address, "/ws/chat?agent=web&session_id=lazy-jsonl").await;
    let start = pre_connect.until("jsonl_pre_connect", "session_start").await;
    assert_eq!(start["resumed"], true);
    assert_eq!(start["message_count"], 2);
    assert_eq!(counting.counts(), (0, 1, 0));
    pre_connect.close().await;

    counting.reset();
    let mut client_a =
        RawWsClient::connect(harness.address, "/ws/chat?agent=web&session_id=lazy-jsonl").await;
    client_a.until("jsonl_a", "session_start").await;
    client_a.send_json(json!({"type": "connect"})).await;
    client_a.until("jsonl_a", "connected").await;
    client_a
        .send_json(json!({"type": "turn_control", "version": 1, "op": "negotiate"}))
        .await;
    client_a.until("jsonl_a", "turn_ready").await;
    assert_eq!(counting.counts(), (1, 1, 0));

    counting.reset();
    let mut client_b =
        RawWsClient::connect(harness.address, "/ws/chat?agent=web&session_id=lazy-jsonl").await;
    client_b.until("jsonl_b", "session_start").await;
    client_b.send_json(json!({"type": "connect"})).await;
    client_b.until("jsonl_b", "connected").await;
    client_b
        .send_json(json!({"type": "turn_control", "version": 1, "op": "negotiate"}))
        .await;
    client_b.until("jsonl_b", "turn_ready").await;
    assert_eq!(counting.counts(), (0, 1, 0));

    client_a.close().await;
    client_b.close().await;
}
