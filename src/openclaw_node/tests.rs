/// Phase 1 integration/unit tests for the openclaw_node module
///
/// Coverage:
///   - protocol: Frame serialization / deserialization round-trips
///   - identity:  Ed25519 key generation, persistence, signature building
///   - client:    full handshake against a local mock OpenClaw gateway
///
/// The mock gateway is a real tokio-tungstenite WS server that runs on a random
/// port and implements the minimum OpenClaw v3 handshake sequence:
///   1. Send connect.challenge event
///   2. Receive and validate connect request
///   3. Send HelloOk response
///   4. Send one tick heartbeat
///   5. Optionally send node.invoke.request, then expect node.invoke.result
///   6. Send shutdown event to let client return cleanly

#[cfg(test)]
mod tests {
    use crate::openclaw_node::{
        client::{NodeMessageHandler, OpenClawClient},
        identity::DeviceIdentity,
        protocol::*,
    };
    use futures_util::{future::BoxFuture, SinkExt, StreamExt};
    use serde_json::json;
    use std::net::SocketAddr;
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message as WsMessage};

    // ─────────────────────────────────────────────────────────────
    // Protocol serialisation tests
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn protocol_event_frame_roundtrip() {
        let frame = Frame::Event(EventFrame {
            event: "tick".to_string(),
            payload: Some(json!({"ts": 1234567890u64})),
            seq: Some(42),
            state_version: None,
        });
        let json = serde_json::to_string(&frame).expect("serialize event");
        let parsed: Frame = serde_json::from_str(&json).expect("deserialize event");
        match parsed {
            Frame::Event(ev) => {
                assert_eq!(ev.event, "tick");
                assert_eq!(ev.seq, Some(42));
            }
            _ => panic!("expected EventFrame"),
        }
    }

    #[test]
    fn protocol_request_frame_roundtrip() {
        let frame = Frame::Request(RequestFrame {
            id: "req-1".to_string(),
            method: "connect".to_string(),
            params: Some(json!({"hello": "world"})),
        });
        let json = serde_json::to_string(&frame).expect("serialize request");
        let parsed: Frame = serde_json::from_str(&json).expect("deserialize request");
        match parsed {
            Frame::Request(req) => {
                assert_eq!(req.id, "req-1");
                assert_eq!(req.method, "connect");
            }
            _ => panic!("expected RequestFrame"),
        }
    }

    #[test]
    fn protocol_response_frame_roundtrip() {
        let frame = Frame::Response(ResponseFrame {
            id: "req-1".to_string(),
            ok: true,
            payload: Some(json!({"type": "hello-ok"})),
            error: None,
        });
        let json = serde_json::to_string(&frame).expect("serialize response");
        let parsed: Frame = serde_json::from_str(&json).expect("deserialize response");
        match parsed {
            Frame::Response(res) => {
                assert!(res.ok);
                assert_eq!(res.id, "req-1");
            }
            _ => panic!("expected ResponseFrame"),
        }
    }

    #[test]
    fn protocol_connect_challenge_parses() {
        let raw = r#"{"type":"event","event":"connect.challenge","payload":{"nonce":"abc123","ts":1700000000}}"#;
        let frame: Frame = serde_json::from_str(raw).expect("parse challenge event");
        match frame {
            Frame::Event(ev) => {
                assert_eq!(ev.event, "connect.challenge");
                let challenge: ConnectChallenge =
                    serde_json::from_value(ev.payload.unwrap()).expect("parse challenge");
                assert_eq!(challenge.nonce, "abc123");
                assert_eq!(challenge.ts, 1700000000);
            }
            _ => panic!("expected event"),
        }
    }

    #[test]
    fn protocol_node_invoke_request_roundtrip() {
        let req = NodeInvokeRequest {
            id: "inv-1".to_string(),
            node_id: "zeroclaw-node-001".to_string(),
            command: "agent.chat".to_string(),
            params_json: Some(r#"{"message":"hello"}"#.to_string()),
            timeout_ms: Some(30000),
            idempotency_key: None,
        };
        let json = serde_json::to_string(&req).expect("serialize invoke request");
        let parsed: NodeInvokeRequest = serde_json::from_str(&json).expect("deserialize invoke request");
        assert_eq!(parsed.id, "inv-1");
        assert_eq!(parsed.command, "agent.chat");
        assert_eq!(parsed.timeout_ms, Some(30000));
    }

    #[test]
    fn protocol_gateway_policy_defaults() {
        let policy: GatewayPolicy = serde_json::from_str("{}").expect("parse empty policy");
        assert_eq!(policy.max_payload, 26214400); // 25 MiB
        assert_eq!(policy.max_buffered_bytes, 52428800); // 50 MiB
        assert_eq!(policy.tick_interval_ms, 30000);
    }

    // ─────────────────────────────────────────────────────────────
    // Device identity tests
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn identity_generates_unique_ids() {
        let a = DeviceIdentity::generate().expect("generate a");
        let b = DeviceIdentity::generate().expect("generate b");
        assert_ne!(a.device_id(), b.device_id(), "device IDs must be unique UUIDs");
        assert_ne!(
            a.public_key_base64url(),
            b.public_key_base64url(),
            "public keys must differ"
        );
    }

    #[test]
    fn identity_persist_and_reload() {
        let tmpdir = TempDir::new().unwrap();
        let path = tmpdir.path().join("subdir").join("device-key.json");

        let id1 = DeviceIdentity::generate().unwrap();
        id1.save(&path).unwrap();

        let id2 = DeviceIdentity::load(&path).unwrap();
        assert_eq!(id1.device_id(), id2.device_id());
        assert_eq!(id1.public_key_base64url(), id2.public_key_base64url());
    }

    #[test]
    fn identity_load_or_create_creates_on_missing_file() {
        let tmpdir = TempDir::new().unwrap();
        let path = tmpdir.path().join("key.json");
        assert!(!path.exists());

        let id = DeviceIdentity::load_or_create(&path).unwrap();
        assert!(!id.device_id().is_empty());
        assert!(path.exists(), "key file should be created");
    }

    #[test]
    fn identity_load_or_create_reloads_existing() {
        let tmpdir = TempDir::new().unwrap();
        let path = tmpdir.path().join("key.json");

        let id1 = DeviceIdentity::load_or_create(&path).unwrap();
        let id2 = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(id1.device_id(), id2.device_id(), "should reload same identity");
    }

    #[test]
    fn identity_v3_signature_non_empty() {
        let id = DeviceIdentity::generate().unwrap();
        let sig = id
            .build_v3_signature(
                "node-host",
                "node",
                "node",
                &["agent"],
                1_700_000_000_000,
                "tok-abc",
                "nonce-xyz",
                "linux",
                None,
            )
            .unwrap();
        assert!(!sig.is_empty(), "signature must not be empty");
        // base64url chars only
        for ch in sig.chars() {
            assert!(
                ch.is_ascii_alphanumeric() || ch == '-' || ch == '_',
                "invalid base64url char: {}",
                ch
            );
        }
    }

    #[test]
    fn identity_v3_signature_differs_with_different_nonce() {
        let id = DeviceIdentity::generate().unwrap();
        let sig1 = id
            .build_v3_signature("node-host", "node", "node", &[], 1_700_000_000_000, "tok", "nonce1", "linux", None)
            .unwrap();
        let sig2 = id
            .build_v3_signature("node-host", "node", "node", &[], 1_700_000_000_000, "tok", "nonce2", "linux", None)
            .unwrap();
        assert_ne!(sig1, sig2, "different nonces must produce different signatures");
    }

    #[test]
    fn identity_v3_signature_stable_for_same_input() {
        // Ed25519 signatures are deterministic
        let id = DeviceIdentity::generate().unwrap();
        let sig1 = id
            .build_v3_signature("node-host", "node", "node", &[], 1_700_000_000_000, "tok", "nonce", "linux", None)
            .unwrap();
        let sig2 = id
            .build_v3_signature("node-host", "node", "node", &[], 1_700_000_000_000, "tok", "nonce", "linux", None)
            .unwrap();
        assert_eq!(sig1, sig2, "Ed25519 signatures must be deterministic");
    }

    // ─────────────────────────────────────────────────────────────
    // Mock gateway helpers
    // ─────────────────────────────────────────────────────────────

    /// Build the minimal HelloOk JSON that the gateway sends back on connect
    fn make_hello_ok_json(req_id: &str) -> String {
        let payload = json!({
            "type": "hello-ok",
            "protocol": PROTOCOL_VERSION,
            "server": { "version": "test-1.0", "connId": "srv-conn-1" },
            "features": { "methods": ["connect","node.pair.request"], "events": ["tick","node.invoke.request","shutdown"] },
            "snapshot": {
                "presence": [],
                "health": {},
                "stateVersion": { "presence": 1, "health": 1 },
                "uptimeMs": 0,
                "authMode": "paired"
            },
            "canvasHostUrl": "https://test.openclaw.ai",
            "auth": {
                "deviceToken": "test-device-token-xyz",
                "role": "node",
                "scopes": [],
                "issuedAtMs": 1_700_000_000_000u64
            },
            "policy": {
                "maxPayload": 26214400,
                "maxBufferedBytes": 52428800,
                "tickIntervalMs": 30000
            }
        });

        let frame = json!({
            "type": "res",
            "id": req_id,
            "ok": true,
            "payload": payload
        });
        frame.to_string()
    }

    fn make_tick_event_json(seq: u64) -> String {
        json!({
            "type": "event",
            "event": "tick",
            "seq": seq,
            "payload": { "ts": 1_700_000_000_000u64 }
        })
        .to_string()
    }

    fn make_shutdown_event_json() -> String {
        json!({ "type": "event", "event": "shutdown" }).to_string()
    }

    fn make_invoke_request_json(inv_id: &str, node_id: &str) -> String {
        json!({
            "type": "event",
            "event": "node.invoke.request",
            "payload": {
                "id": inv_id,
                "nodeId": node_id,
                "command": "agent.chat",
                "paramsJson": r#"{"message":"hello from gateway"}"#
            }
        })
        .to_string()
    }

    /// Spawn a mock OpenClaw gateway on a random port.
    /// Returns the bound address.  The server:
    ///   1. Accepts one connection
    ///   2. Sends connect.challenge
    ///   3. Reads connect request (validates method == "connect")
    ///   4. Sends HelloOk
    ///   5. Sends tick
    ///   6. Optionally sends invoke, reads result
    ///   7. Sends shutdown
    async fn spawn_mock_gateway(with_invoke: bool) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();

            // 1. Send challenge
            let challenge = json!({
                "type": "event",
                "event": "connect.challenge",
                "payload": { "nonce": "test-nonce-123", "ts": 1_700_000_000_000u64 }
            })
            .to_string();
            ws.send(WsMessage::Text(challenge.into())).await.unwrap();

            // 2. Read connect request
            let msg = ws.next().await.unwrap().unwrap();
            let frame: Frame = serde_json::from_str(&msg.to_string()).unwrap();
            let req_id = match &frame {
                Frame::Request(r) => {
                    assert_eq!(r.method, "connect", "first client request must be 'connect'");
                    // Validate params are present
                    assert!(r.params.is_some(), "connect must have params");
                    r.id.clone()
                }
                _ => panic!("expected Request frame for connect"),
            };

            // 3. Send HelloOk
            ws.send(WsMessage::Text(make_hello_ok_json(&req_id).into()))
                .await
                .unwrap();

            // 4. Send tick
            ws.send(WsMessage::Text(make_tick_event_json(1).into()))
                .await
                .unwrap();

            // 5. Optionally: send invoke, read result
            if with_invoke {
                ws.send(WsMessage::Text(
                    make_invoke_request_json("inv-test-1", "zeroclaw-test-node").into(),
                ))
                .await
                .unwrap();

                // Drain messages until we see node.invoke.result
                let result_msg = loop {
                    let msg = ws.next().await.unwrap().unwrap();
                    let frame: Frame = serde_json::from_str(&msg.to_string()).unwrap();
                    if let Frame::Request(r) = &frame {
                        if r.method == "node.invoke.result" {
                            break r.clone();
                        }
                    }
                };
                // Validate result
                let params = result_msg.params.unwrap();
                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                assert_eq!(id, "inv-test-1", "result id must match invoke id");
            }

            // 6. Send shutdown to terminate client
            ws.send(WsMessage::Text(make_shutdown_event_json().into()))
                .await
                .unwrap();
        });

        addr
    }

    // ─────────────────────────────────────────────────────────────
    // Stub handler for tests
    // ─────────────────────────────────────────────────────────────

    struct TestHandler {
        node_id: String,
        connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
        invoke_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl TestHandler {
        fn new(node_id: &str) -> Self {
            TestHandler {
                node_id: node_id.to_string(),
                connected: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                invoke_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            }
        }
    }

    impl NodeMessageHandler for TestHandler {
        fn on_invoke(
            &self,
            req: NodeInvokeRequest,
        ) -> BoxFuture<'static, crate::openclaw_node::client::NodeInvokeResult> {
            let node_id = self.node_id.clone();
            let count = self.invoke_count.clone();
            Box::pin(async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::openclaw_node::client::NodeInvokeResult {
                    id: req.id,
                    node_id,
                    ok: true,
                    payload_json: Some(r#"{"reply":"ok from zeroclaw"}"#.to_string()),
                    error: None,
                }
            })
        }

        fn on_connected(&self) {
            self.connected
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn on_disconnected(&self) {}
    }

    // ─────────────────────────────────────────────────────────────
    // Client integration tests
    // ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn client_handshake_succeeds_against_mock_gateway() {
        let addr = spawn_mock_gateway(false).await;
        let gateway_url = format!("ws://{}", addr);

        let identity = DeviceIdentity::generate().expect("gen identity");
        let handler = TestHandler::new("zeroclaw-test-node");
        let connected = handler.connected.clone();

        let mut client = OpenClawClient::new(
            &gateway_url,
            "zeroclaw-test-node",
            "ZeroClaw Test Node",
            identity,
            Some("test-gateway-token".to_string()),
        );

        // run_once will return Err("shutdown") — that is the expected clean exit
        let result = client.run_once(&handler).await;

        assert!(
            connected.load(std::sync::atomic::Ordering::SeqCst),
            "on_connected must have been called"
        );
        match result {
            Err(e) => {
                assert!(
                    e.to_string().contains("shutdown"),
                    "expected shutdown error, got: {}",
                    e
                );
            }
            Ok(_) => {
                // Also acceptable — client may return Ok on shutdown
            }
        }
    }

    #[tokio::test]
    async fn client_processes_invoke_request() {
        let addr = spawn_mock_gateway(true).await;
        let gateway_url = format!("ws://{}", addr);

        let identity = DeviceIdentity::generate().expect("gen identity");
        let handler = TestHandler::new("zeroclaw-test-node");
        let invoke_count = handler.invoke_count.clone();

        let mut client = OpenClawClient::new(
            &gateway_url,
            "zeroclaw-test-node",
            "ZeroClaw Test Node",
            identity,
            None,
        );

        let _ = client.run_once(&handler).await;

        assert_eq!(
            invoke_count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "on_invoke must have been called exactly once"
        );
    }

    #[tokio::test]
    async fn client_fails_gracefully_when_no_server() {
        // Port 1 is reserved and will refuse connections
        let identity = DeviceIdentity::generate().expect("gen identity");
        let handler = TestHandler::new("zeroclaw-test-node");

        let mut client = OpenClawClient::new(
            "ws://127.0.0.1:1",
            "zeroclaw-test-node",
            "ZeroClaw Test Node",
            identity,
            None,
        );

        let result = client.run_once(&handler).await;
        assert!(result.is_err(), "connecting to refused port must fail");
    }

    #[tokio::test]
    async fn client_handshake_requires_challenge_first() {
        // Mock gateway that sends the wrong event first (not connect.challenge)
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            // Send wrong event
            let wrong = json!({"type": "event", "event": "tick", "payload": {"ts": 0}}).to_string();
            ws.send(WsMessage::Text(wrong.into())).await.unwrap();
            // Drain remaining messages
            while ws.next().await.is_some() {}
        });

        let identity = DeviceIdentity::generate().unwrap();
        let handler = TestHandler::new("node");

        let mut client = OpenClawClient::new(
            &format!("ws://{}", addr),
            "node",
            "Test",
            identity,
            None,
        );
        let result = client.run_once(&handler).await;
        assert!(result.is_err(), "client should reject non-challenge first event");
    }
}
