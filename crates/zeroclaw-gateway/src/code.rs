//! Browser code sessions over the daemon's existing local RPC transport.
//! The bridge admits only code-session methods; configuration, certificates,
//! environment forwarding and arbitrary local endpoints are not exposed.

use super::AppState;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const MAX_FRAME: usize = 8 * 1024 * 1024;
const PROTOCOL: &str = "zeroclaw.code.v1";

fn authenticated(pairing: &zeroclaw_config::pairing::PairingGuard, token: &str) -> bool {
    pairing.require_pairing() && !token.is_empty() && pairing.is_authenticated(token)
}

#[cfg(unix)]
type LocalStream = tokio::net::UnixStream;
#[cfg(windows)]
type LocalStream = tokio::net::windows::named_pipe::NamedPipeClient;

async fn connect_local(state: &AppState) -> std::io::Result<LocalStream> {
    let path = zeroclaw_runtime::rpc::local::socket_path(&state.config.read());
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(path).await
    }
    #[cfg(windows)]
    {
        tokio::net::windows::named_pipe::ClientOptions::new().open(path)
    }
}

/// Probe the actual listener, without initializing or creating a session.
pub async fn available(state: &AppState) -> bool {
    state.tui_registry.is_some()
        && state.pairing.require_pairing()
        && state.pairing.is_paired()
        && matches!(
            tokio::time::timeout(std::time::Duration::from_millis(250), connect_local(state)).await,
            Ok(Ok(_))
        )
}

pub async fn handle_ws_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("sec-websocket-protocol")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.split(',')
                        .map(str::trim)
                        .find_map(|v| v.strip_prefix("bearer."))
                })
        })
        .unwrap_or("");
    // Local IPC carries more authority than an anonymous HTTP gateway. This
    // surface always requires an issued token, even on pairing-disabled hosts.
    if !authenticated(&state.pairing, token) {
        return (
            StatusCode::UNAUTHORIZED,
            "Code sessions require a paired gateway token",
        )
            .into_response();
    }
    if state.tui_registry.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Code sessions require the daemon",
        )
            .into_response();
    }
    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        connect_local(&state),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        _ => return (StatusCode::SERVICE_UNAVAILABLE, "Local RPC is unavailable").into_response(),
    };
    ws.protocols([PROTOCOL])
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| bridge(socket, stream))
        .into_response()
}

fn code_frame(text: &str) -> Result<String, Value> {
    let mut frame: Value = serde_json::from_str(text).map_err(
        |_| json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON"}}),
    )?;
    let id = frame.get("id").cloned().unwrap_or(Value::Null);
    let error = || json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method unavailable in the code workspace"}});
    // Strict envelope validation prevents batches/ambiguous frames from
    // slipping past the method boundary. RPC remains the parameter validator.
    zeroclaw_api::jsonrpc::JsonRpcFrame::from_value(frame.clone()).map_err(|_| error())?;
    let Some(method) = frame.get("method").and_then(Value::as_str) else {
        // Responses to daemon-initiated elicitation go back to their owner.
        return Ok(frame.to_string());
    };
    match method {
        "initialize" => {
            let params = frame.get("params").cloned().unwrap_or(json!({}));
            // Identity can only be reclaimed with the runtime's signature.
            frame["params"] = json!({
                "protocol_version": 1,
                "tui_id": params.get("tui_id"),
                "tui_sig": params.get("tui_sig"),
                "clientCapabilities": params.get("clientCapabilities"),
            });
        }
        "session/new" => {
            let params = frame.get("params").cloned().unwrap_or(json!({}));
            // Match zerocode's code pane. Workspace resolution and tool policy
            // stay server-owned; browsers cannot forward a shell environment.
            frame["params"] = json!({
                "agent_alias": params.get("agent_alias"),
                "session_id": params.get("session_id"),
                "exclude_memory": true,
                "chat_mode": "acp",
                "keep_siblings": true,
                "interaction_surface": "zerocode_code",
            });
        }
        "session/prompt" | "session/cancel" | "session/approve" | "session/messages"
        | "session/state" | "session/list-acp" => {}
        _ => return Err(error()),
    }
    Ok(frame.to_string())
}

async fn bridge<S>(socket: WebSocket, stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, mut receiver) = socket.split();
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let (error_tx, mut errors) = tokio::sync::mpsc::channel::<String>(8);
    let input = async {
        while let Some(Ok(message)) = receiver.next().await {
            match message {
                Message::Text(text) => match code_frame(&text) {
                    Ok(frame) => {
                        if write
                            .write_all(format!("{frame}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        if error_tx.send(error.to_string()).await.is_err() {
                            break;
                        }
                    }
                },
                Message::Close(_) => break,
                Message::Binary(_) => break,
                Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    };
    let output = async {
        let mut line = Vec::new();
        loop {
            let mut bounded = (&mut reader).take((MAX_FRAME + 1 - line.len()) as u64);
            let text = tokio::select! {
                result = bounded.read_until(b'\n', &mut line) => {
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line.len() > MAX_FRAME => break,
                        Ok(_) => match String::from_utf8(std::mem::take(&mut line)) { Ok(line) => line, Err(_) => break },
                    }
                }
                error = errors.recv() => match error { Some(error) => error, None => break },
            };
            if sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    };
    // EOF on either side drops the local connection. Its existing runtime
    // teardown cancels/drains prompts and retains code history canonically.
    tokio::select! { _ = input => {}, _ = output => {} }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_auth_stays_closed_when_gateway_pairing_is_disabled() {
        use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};
        for required in [true, false] {
            let guard = PairingGuard::new(
                required,
                &["test-token".into()],
                PairingCodePolicy::default(),
            );
            assert!(!authenticated(&guard, ""));
            assert!(!authenticated(&guard, "incorrect-token"));
            assert_eq!(authenticated(&guard, "test-token"), required);
        }
    }

    #[test]
    fn code_transport_rejects_privileged_methods_and_batches() {
        for method in [
            "config/set",
            "cert/issue",
            "session/delete",
            "session/kill",
            "status",
        ] {
            assert!(
                code_frame(&json!({"jsonrpc":"2.0","id":1,"method":method}).to_string()).is_err()
            );
        }
        assert!(code_frame(r#"[{"jsonrpc":"2.0","id":1,"method":"initialize"}]"#).is_err());
    }

    #[test]
    fn code_session_uses_canonical_workspace_and_surface() {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"session/new","params":{
            "agent_alias":"test-agent","cwd":"/outside","chat_mode":"chat","exclude_memory":false,"tui_id":"another-client"
        }});
        let frame: Value =
            serde_json::from_str(&code_frame(&request.to_string()).unwrap()).unwrap();
        assert_eq!(frame["params"]["chat_mode"], "acp");
        assert_eq!(frame["params"]["interaction_surface"], "zerocode_code");
        assert_eq!(frame["params"]["exclude_memory"], true);
        assert!(frame["params"].get("cwd").is_none());
        assert!(frame["params"].get("tui_id").is_none());
    }

    #[test]
    fn initialization_never_forwards_a_browser_environment() {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"env":{"PATH":"/outside"}}});
        let frame: Value =
            serde_json::from_str(&code_frame(&request.to_string()).unwrap()).unwrap();
        assert!(frame["params"].get("env").is_none());
    }

    /// Exercise the production HTTP auth/upgrade, local IPC, runtime dispatcher
    /// and persisted code history with an isolated install and no model calls.
    #[cfg(unix)]
    #[tokio::test]
    async fn code_front_door_auth_session_history_and_activity() {
        use std::sync::{Arc, atomic::AtomicUsize};
        use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, AnthropicModelProviderConfig, Config, ModelProviderConfig,
            RiskProfileConfig, RuntimeProfileConfig,
        };
        use zeroclaw_runtime::rpc::{context::RpcContext, session::SessionStore};
        let temp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: temp.path().join("data"),
            config_path: temp.path().join("config.toml"),
            ..Config::default()
        };
        config.gateway.paired_tokens = vec!["zc_test_code_token".into()];
        config.providers.models.anthropic.insert(
            "default".into(),
            AnthropicModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("test-model".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        config
            .risk_profiles
            .insert("default".into(), RiskProfileConfig::default());
        config
            .runtime_profiles
            .insert("default".into(), RuntimeProfileConfig::default());
        config.agents.insert(
            "test-agent".into(),
            AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                risk_profile: "default".into(),
                runtime_profile: "default".into(),
                ..Default::default()
            },
        );
        let expected = config.agent_workspace_dir("test-agent");
        let store = Arc::new(SessionStore::new(
            16,
            Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
                4, 10, 60,
            )),
        ));
        let context = RpcContext::for_live_test(config.clone(), store.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        let listener_context = context.clone();
        let listener_cancel = cancel.clone();
        let listener = zeroclaw_spawn::spawn!(zeroclaw_runtime::rpc::local::run_local_listener(
            listener_context,
            listener_cancel,
            Arc::new(AtomicUsize::new(0)),
            None
        ));
        let socket_path = zeroclaw_runtime::rpc::local::socket_path(&config);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !socket_path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (reload_tx, _) = tokio::sync::watch::channel(false);
        let gateway_shutdown = shutdown_tx.clone();
        let gateway = zeroclaw_spawn::spawn!(crate::run_gateway(
            "127.0.0.1",
            port,
            config,
            None,
            Some(zeroclaw_runtime::daemon::GatewayReloadControls::standalone(
                gateway_shutdown,
                reload_tx
            )),
            Some(context.tui_registry.clone()),
            None,
            None,
            None,
            None,
            None,
            None
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let url = format!("ws://127.0.0.1:{port}/ws/code");
        let mut http = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        http.write_all(b"GET /api/workspace HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer zc_test_code_token\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            http.read_to_string(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let availability: Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(availability["code"], true);
        assert_eq!(availability["workflows"], false);
        for unauthenticated_url in [&url, &format!("{url}?token=zc_test_code_token")] {
            let denied = tokio_tungstenite::connect_async(unauthenticated_url)
                .await
                .unwrap_err();
            match denied {
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED)
                }
                other => panic!("expected HTTP auth refusal, got {other}"),
            }
        }
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            "zeroclaw.code.v1, bearer.zc_test_code_token"
                .parse()
                .unwrap(),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        async fn rpc<S>(
            ws: &mut tokio_tungstenite::WebSocketStream<S>,
            id: i64,
            method: &str,
            params: Value,
        ) -> Value
        where
            S: AsyncRead + AsyncWrite + Unpin,
        {
            ws.send(ClientMessage::Text(
                json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while let Some(Ok(ClientMessage::Text(text))) = ws.next().await {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["id"] == id {
                        return value;
                    }
                }
                panic!("RPC connection closed before response");
            })
            .await
            .unwrap()
        }
        assert!(
            rpc(&mut ws, 1, "initialize", json!({}))
                .await
                .get("result")
                .is_some()
        );
        assert!(
            rpc(&mut ws, 2, "config/set", json!({}))
                .await
                .get("error")
                .is_some()
        );
        let created = rpc(
            &mut ws,
            3,
            "session/new",
            json!({"agent_alias":"test-agent","cwd":"/outside"}),
        )
        .await;
        assert!(created.get("error").is_none(), "{created}");
        assert_eq!(
            created["result"]["workspace_dir"],
            expected.canonicalize().unwrap().to_string_lossy().as_ref()
        );
        let sid = created["result"]["session_id"].as_str().unwrap();
        let turn = tokio_util::sync::CancellationToken::new();
        let generation = store.register_cancel_token(sid, turn);
        let activity = rpc(&mut ws, 4, "session/list-acp", json!({})).await;
        assert_eq!(activity["result"]["sessions"][0]["state"], "running");
        assert_eq!(
            activity["result"]["sessions"][0]["interaction_surface"],
            "zerocode_code"
        );
        store.remove_cancel_token(sid, generation);
        let activity = rpc(&mut ws, 5, "session/list-acp", json!({})).await;
        assert_eq!(activity["result"]["sessions"][0]["state"], "idle");
        assert!(rpc(&mut ws, 6, "session/messages", json!({"session_id":sid})).await["result"]["messages"].is_array());
        ws.close(None).await.unwrap();
        cancel.cancel();
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), listener)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), gateway)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
