/// OpenClaw WebSocket client — manages connection, handshake, and message routing
use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage, WebSocketStream};
use url::Url;

use super::identity::DeviceIdentity;
use super::protocol::*;

pub const CONNECT_TIMEOUT_SECS: u64 = 10;
pub const HANDSHAKE_TIMEOUT_SECS: u64 = 5;

pub trait NodeMessageHandler: Send + Sync {
    /// Handle an invoke request from the gateway
    fn on_invoke(&self, req: NodeInvokeRequest) -> futures_util::future::BoxFuture<'static, NodeInvokeResult>;

    /// Called when connection is established
    fn on_connected(&self);

    /// Called when connection is lost
    fn on_disconnected(&self);
}

pub struct NodeInvokeResult {
    pub id: String,
    pub node_id: String,
    pub ok: bool,
    pub payload_json: Option<String>,
    pub error: Option<ErrorDetail>,
}

pub struct OpenClawClient {
    gateway_url: String,
    node_id: String,
    display_name: String,
    device_identity: DeviceIdentity,
    gateway_token: Option<String>,
    device_token: Option<String>,
    last_tick_seq: Option<u64>,
}

impl OpenClawClient {
    pub fn new(
        gateway_url: impl Into<String>,
        node_id: impl Into<String>,
        display_name: impl Into<String>,
        device_identity: DeviceIdentity,
        gateway_token: Option<String>,
    ) -> Self {
        OpenClawClient {
            gateway_url: gateway_url.into(),
            node_id: node_id.into(),
            display_name: display_name.into(),
            device_identity,
            gateway_token,
            device_token: None,
            last_tick_seq: None,
        }
    }

    /// Connect to gateway, perform handshake, and enter message loop
    pub async fn run(&mut self, handler: Box<dyn NodeMessageHandler>) -> Result<()> {
        loop {
            match self.connect_and_run(handler.as_ref()).await {
                Ok(_) => {
                    // Normal exit
                    break;
                }
                Err(e) => {
                    eprintln!("openclaw node connection error: {}, reconnecting...", e);
                    handler.on_disconnected();
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
        Ok(())
    }

    /// Connect once, perform handshake after message loop — no reconnect loop.
    /// Used in tests to avoid the infinite retry loop.
    pub async fn run_once(&mut self, handler: &dyn NodeMessageHandler) -> Result<()> {
        self.connect_and_run(handler).await
    }

    async fn connect_and_run(&mut self, handler: &dyn NodeMessageHandler) -> Result<()> {
        // Parse URL and connect
        let _url = Url::parse(&self.gateway_url)
            .map_err(|e| anyhow!("invalid gateway URL: {}", e))?;

        let (ws_stream, _) = timeout(
            Duration::from_secs(CONNECT_TIMEOUT_SECS),
            connect_async(&self.gateway_url),
        )
        .await
        .map_err(|_| anyhow!("gateway connection timeout"))?
        .map_err(|e| anyhow!("failed to connect to gateway: {}", e))?;

        eprintln!("connected to gateway: {}", self.gateway_url);

        let mut client_state = ClientState {
            ws_stream,
            node_id: self.node_id.clone(),
            device_identity: self.device_identity.clone(),
            gateway_token: self.gateway_token.clone(),
            device_token: self.device_token.clone(),
            display_name: self.display_name.clone(),
            next_request_id: 1,
            last_tick_ts: None,
            last_tick_seq: None,
            tick_stall_threshold_ms: TICK_INTERVAL_MS * TICK_STALL_MULTIPLIER,
        };

        // Perform handshake
        let hello_ok = client_state.handshake().await?;
        self.device_token = Some(hello_ok.auth.device_token.clone());
        handler.on_connected();

        // Enter message loop
        client_state.message_loop(handler).await?;

        Ok(())
    }
}

struct ClientState {
    ws_stream: WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    node_id: String,
    device_identity: DeviceIdentity,
    gateway_token: Option<String>,
    device_token: Option<String>,
    display_name: String,
    next_request_id: u64,
    last_tick_ts: Option<u64>,
    last_tick_seq: Option<u64>,
    tick_stall_threshold_ms: u64,
}

impl ClientState {
    /// Perform connect handshake: wait for challenge, send connect, receive HelloOk
    async fn handshake(&mut self) -> Result<HelloOk> {
        // Wait for connect.challenge event
        let nonce = loop {
            let msg = timeout(Duration::from_secs(HANDSHAKE_TIMEOUT_SECS), self.ws_stream.next())
                .await
                .map_err(|_| anyhow!("handshake timeout"))?
                .ok_or(anyhow!("gateway closed connection before challenge"))?
                .map_err(|e| anyhow!("websocket error: {}", e))?;

            let frame: Frame = serde_json::from_str(&msg.to_string())
                .map_err(|e| anyhow!("failed to parse connect.challenge: {}", e))?;

            match frame {
                Frame::Event(ev) => {
                    if ev.event == "connect.challenge" {
                        let challenge: ConnectChallenge = serde_json::from_value(
                            ev.payload.ok_or(anyhow!("missing challenge payload"))?,
                        )
                        .map_err(|e| anyhow!("failed to parse challenge: {}", e))?;
                        break challenge.nonce;
                    } else {
                        return Err(anyhow!("unexpected event before connect: {}", ev.event));
                    }
                }
                _ => {
                    return Err(anyhow!("expected event frame for challenge"));
                }
            }
        };

        // Build and send connect request
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let platform = std::env::consts::OS.to_string();
        let signature = self
            .device_identity
            .build_v3_signature("node-host", "node", "node", &[], now_ms, &self.gateway_token.as_deref().unwrap_or(""), &nonce, &platform, None)?;

        let connect_req = RequestFrame {
            id: self.next_request_id().to_string(),
            method: "connect".to_string(),
            params: Some(serde_json::to_value(ConnectParams {
                min_protocol: PROTOCOL_VERSION,
                max_protocol: PROTOCOL_VERSION,
                client: ClientInfo {
                    id: "node-host".to_string(),
                    display_name: self.display_name.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    platform,
                    mode: "node".to_string(),
                    instance_id: self.node_id.clone(),
                },
                caps: vec![],
                commands: vec![],
                role: "node".to_string(),
                scopes: vec![],
                device: DeviceAuth {
                    id: self.device_identity.device_id().to_string(),
                    public_key: self.device_identity.public_key_base64url(),
                    signature,
                    signed_at: now_ms,
                    nonce,
                },
                auth: AuthCredentials {
                    token: self.gateway_token.clone(),
                    device_token: self.device_token.clone(),
                    password: None,
                },
                permissions: None,
                path_env: None,
            })?),
        };

        let frame = Frame::Request(connect_req);
        let msg = WsMessage::Text(serde_json::to_string(&frame)?.into());
        self.ws_stream.send(msg).await
            .map_err(|e| anyhow!("failed to send connect request: {}", e))?;

        // Wait for connect response (HelloOk)
        let hello_ok = timeout(Duration::from_secs(HANDSHAKE_TIMEOUT_SECS), self.ws_stream.next())
            .await
            .map_err(|_| anyhow!("timeout waiting for connect response"))?
            .ok_or(anyhow!("gateway closed after connect"))?
            .map_err(|e| anyhow!("websocket error: {}", e))?;

        let frame: Frame = serde_json::from_str(&hello_ok.to_string())
            .map_err(|e| anyhow!("failed to parse connect response: {}", e))?;

        match frame {
            Frame::Response(res) => {
                if !res.ok {
                    return Err(anyhow!("connect failed: {:?}", res.error));
                }
                let hello_ok: HelloOk = serde_json::from_value(
                    res.payload.ok_or(anyhow!("missing HelloOk payload"))?,
                )?;
                Ok(hello_ok)
            }
            _ => Err(anyhow!("expected response frame for connect")),
        }
    }

    /// Main message loop: receive and route events/responses
    async fn message_loop(&mut self, handler: &dyn NodeMessageHandler) -> Result<()> {
        while let Some(msg) = self.ws_stream.next().await {
            let msg = msg.map_err(|e| anyhow!("websocket error: {}", e))?;
            let frame: Frame = serde_json::from_str(&msg.to_string())
                .map_err(|e| anyhow!("failed to parse frame: {}", e))?;

            match frame {
                Frame::Event(ev) => {
                    self.handle_event(&ev, handler).await?;
                }
                Frame::Response(res) => {
                    self.handle_response(&res).await?;
                }
                _ => {
                    eprintln!("unexpected frame type: {:?}", frame);
                }
            }
        }
        Err(anyhow!("gateway closed connection"))
    }

    async fn handle_event(&mut self, ev: &EventFrame, handler: &dyn NodeMessageHandler) -> Result<()> {
        match ev.event.as_str() {
            "tick" => {
                if let Some(payload) = &ev.payload {
                    let tick: Tick = serde_json::from_value(payload.clone())?;
                    self.last_tick_ts = Some(tick.ts);
                }
                if let Some(seq) = ev.seq {
                    self.last_tick_seq = Some(seq);
                }
            }
            "node.invoke.request" => {
                if let Some(payload) = &ev.payload {
                    let req: NodeInvokeRequest = serde_json::from_value(payload.clone())?;
                    let result = handler.on_invoke(req).await;
                    self.send_invoke_result(result).await?;
                }
            }
            "shutdown" => {
                eprintln!("gateway shutdown signal received");
                return Err(anyhow!("shutdown"));
            }
            event => {
                eprintln!("unhandled event: {}", event);
            }
        }
        Ok(())
    }

    async fn handle_response(&mut self, _res: &ResponseFrame) -> Result<()> {
        // For now, ignore responses (they are acks)
        Ok(())
    }

    async fn send_invoke_result(&mut self, result: NodeInvokeResult) -> Result<()> {
        let req = RequestFrame {
            id: self.next_request_id().to_string(),
            method: "node.invoke.result".to_string(),
            params: Some(serde_json::to_value(NodeInvokeResultParams {
                id: result.id,
                node_id: result.node_id,
                ok: result.ok,
                payload_json: result.payload_json,
                error: result.error,
            })?),
        };
        let frame = Frame::Request(req);
        let msg = WsMessage::Text(serde_json::to_string(&frame)?.into());
        self.ws_stream.send(msg).await
            .map_err(|e| anyhow!("failed to send invoke result: {}", e))?;
        Ok(())
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }
}
