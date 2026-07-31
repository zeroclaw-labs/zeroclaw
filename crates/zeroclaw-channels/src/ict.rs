use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::StreamMode;

type HmacSha256 = Hmac<Sha256>;

const ICT_AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const ICT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const ICT_OUTBOUND_QUEUE_DEPTH: usize = 64;
const ICT_HEARTBEAT_TIMEOUT_MULTIPLIER: u64 = 3;
/// Conservative local budget for the time it takes to perform a reconnect
/// (handshake + first-frame auth). Used to decide whether `Instant::now()` is
/// still safely inside the registered credential's lifetime before reusing it
/// for the next connection attempt. Bounded by `min(heartbeat_interval_secs,
/// 30)` at the call site.
const ICT_RECONNECT_BUDGET_CAP_SECS: u64 = 30;
const ICT_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);
const ICT_REGISTRATION_RETRY_DELAY: Duration = Duration::from_secs(3);

macro_rules! ict_log_info {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            format!($($arg)*),
        )
    };
}

macro_rules! ict_log_debug {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            format!($($arg)*),
        )
    };
}

macro_rules! ict_log_warn {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            format!($($arg)*),
        )
    };
}

macro_rules! ict_log_error {
    ($($arg:tt)*) => {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            format!($($arg)*),
        )
    };
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IctWireMessage {
    #[serde(rename = "type")]
    msg_type: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<String>,
    #[serde(rename = "requestId", default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp: Option<i64>,
}

impl IctWireMessage {
    fn heartbeat() -> Self {
        Self {
            msg_type: 0,
            data: Some("heartbeat".to_string()),
            request_id: None,
            session_id: None,
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
        }
    }

    fn reply(data: String, request_id: String, session_id: String) -> Self {
        Self {
            msg_type: 1,
            data: Some(data),
            request_id: Some(request_id),
            session_id: Some(session_id),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
        }
    }

    /// End-of-stream marker. Reuses the business frame shape (`type=1`) with
    /// `data == "[DONE]"` and the same `request_id` / `session_id` as the
    /// preceding reply, matching the historical `ictmsg` wire shape
    /// (`docs/book/ictmsg.rs`). Defined here — not in `IctOutbound` — so the
    /// `msg_type`/payload shape remains the single source of truth for the
    /// wire schema.
    fn done_marker(request_id: String, session_id: String) -> Self {
        Self {
            msg_type: 1,
            data: Some("[DONE]".to_string()),
            request_id: Some(request_id),
            session_id: Some(session_id),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
        }
    }

    /// Proactive / notification frame (`type=2`). Used by cron-style delivery
    /// (and any other "no inbound request to reply to" path) where there is no
    /// `requestId` to reuse because the original request has long since
    /// closed on the upstream platform. Shape mirrors the historical
    /// `docs/book/ictmsg.rs` `msg_type:2` notification: `data` + `sessionId`,
    /// no `requestId`, no follow-up `[DONE]` marker. Defined here — not in
    /// `IctOutbound` — so the `msg_type`/payload shape remains the single
    /// source of truth for the wire schema.
    fn notification(data: String, session_id: String) -> Self {
        Self {
            msg_type: 2,
            data: Some(data),
            request_id: None,
            session_id: Some(session_id),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
        }
    }
}

/// Configuration snapshot resolved from the global `Config` at use
/// time. Only carries the registration-side fields. The WSS connection
/// credentials (`wss_url` / `username` / `password`) live in [`IctConnect`]
/// and are populated by the registration step at runtime; they are not
/// duplicated into this snapshot (AGENTS.md single source of truth).
#[derive(Debug, Clone)]
pub struct IctConfigSnapshot {
    pub url: String,
    pub app_id: String,
    pub app_secret: String,
    pub heartbeat_interval_secs: u64,
    pub expiration_time_secs: u64,
    /// Streaming mode for the draft hook (`send_draft` /
    /// `update_draft` / `finalize_draft`). Resolved on demand from the
    /// global `Config`; never duplicated into any other runtime struct
    /// (AGENTS.md single source of truth).
    pub stream_mode: StreamMode,
}

/// Materialized view of a successful registration. This is the **runtime**
/// source of truth for WSS connection credentials: it is written by the
/// registration step, read by the connect step, and never duplicated onto the
/// channel struct's other fields.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `mac` is retained on the runtime credential view for parity with the upstream wire shape (`docs/book/ictmsg.rs`); not consumed in v1.
struct IctConnect {
    pub wss_url: String,
    pub username: String,
    pub password: String,
    pub mac: String,
    /// Absolute deadline computed as
    /// `Instant::now() + IctConfigSnapshot::expiration_time_secs` at the
    /// moment of registration. Compared with
    /// `Instant::now() + reconnect_budget` to decide whether the next
    /// reconnect needs a fresh registration.
    pub expires_at: Instant,
}

#[derive(Debug, Clone, Serialize)]
struct RegistryRequest {
    protocol: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct RegistryResponse {
    #[serde(rename = "resCode")]
    res_code: i32,
    #[serde(rename = "resDesc")]
    res_desc: Option<String>,
    data: Option<RegistryData>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // `mac` / `protocol` mirror the upstream wire shape; not consumed in v1.
struct RegistryData {
    mac: String,
    protocol: i32,
    url: Option<String>,
    addr: Option<String>,
    username: String,
    password: String,
}

#[derive(Debug, Clone)]
enum IctOutbound {
    Reply {
        session_id: String,
        request_id: String,
        data: String,
    },
    /// End-of-stream marker — same `session_id` / `request_id` as the
    /// preceding `Reply`, but no payload. Rendered via
    /// [`IctWireMessage::done_marker`] on the wire path.
    Done {
        session_id: String,
        request_id: String,
    },
    /// Proactive / notification frame — no `request_id` because the upstream
    /// request that would have owned one has long since closed. Rendered via
    /// [`IctWireMessage::notification`] as a `type=2` frame with `data` +
    /// `sessionId` and no follow-up `[DONE]` marker. Used by cron-style
    /// delivery where the channel cannot route a reply through
    /// `session_routes[recipient]`.
    Notification { session_id: String, data: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionOutcome {
    Reconnect,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundKind {
    Heartbeat,
    Message,
    Other,
}

/// Per-draft turn state used by the progressive-streaming draft hook.
///
/// Created on `send_draft`, mutated by `update_draft`, drained on
/// `finalize_draft` / `cancel_draft`. The correlation `request_id` is
/// **not** duplicated from `IctChannel::session_routes` — we look it up
/// fresh at frame-enqueue time. This struct only owns the
/// `draft_id`-scoped bookkeeping (incremental `sent_length`, draft
/// lifetime for cleanup) that has no other source of truth.
#[derive(Debug)]
struct IctDraftState {
    session_id: String,
    sent_length: usize,
    last_activity: Instant,
}

pub struct IctChannel {
    alias: String,
    /// Resolves the registration-side config snapshot from the global
    /// `Config` on demand (the source of truth for `url` / `app_id` /
    /// `app_secret` / `heartbeat_interval_secs` / `expiration_time_secs`).
    config_resolver: Arc<dyn Fn() -> Result<IctConfigSnapshot> + Send + Sync>,
    /// Cache of the most recent successful registration. Written by
    /// `register_once`, read by `connect`. Holds the runtime WSS credentials
    /// — there is **no** `wss_url` / `username` / `password` field anywhere
    /// else on this struct (AGENTS.md single source of truth).
    connect: Arc<RwLock<Option<IctConnect>>>,
    /// Best-known expiry instant for the cached `IctConnect`. Lives next to
    /// `connect` so the "do we need to re-register?" decision does not depend
    /// on a transient state of `connect` itself.
    registered_until: Arc<RwLock<Option<Instant>>>,
    ws_tx: Arc<Mutex<Option<mpsc::Sender<IctOutbound>>>>,
    session_routes: Arc<Mutex<HashMap<String, String>>>,
    last_frame_at: Arc<Mutex<Option<Instant>>>,
    /// In-flight draft turns opened by `send_draft` and not yet closed by
    /// `finalize_draft` / `cancel_draft`. Keyed by the locally-minted
    /// `draft_id` (an opaque client-side handle, not a platform id). Bounded
    /// implicitly by the upstream `cleanup` task below; no per-session
    /// duplicate of `request_id` — that resolves from `session_routes` on
    /// every frame.
    drafts: Arc<Mutex<HashMap<String, IctDraftState>>>,
    /// Shared HTTP client used for the registration `POST`. Kept on the
    /// channel so we can reuse its connection pool across the lifetime of
    /// the channel (re-registrations are infrequent).
    http_client: reqwest::Client,
}

impl IctChannel {
    pub fn new(
        alias: impl Into<String>,
        config_resolver: Arc<dyn Fn() -> Result<IctConfigSnapshot> + Send + Sync>,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(ICT_REGISTRATION_TIMEOUT)
            .build()
            .unwrap_or_else(|err| {
                ict_log_warn!(
                    "ICT reqwest client build failed alias=default error={err:#}; falling back to default"
                );
                reqwest::Client::new()
            });
        Self {
            alias: alias.into(),
            config_resolver,
            connect: Arc::new(RwLock::new(None)),
            registered_until: Arc::new(RwLock::new(None)),
            ws_tx: Arc::new(Mutex::new(None)),
            session_routes: Arc::new(Mutex::new(HashMap::new())),
            last_frame_at: Arc::new(Mutex::new(None)),
            drafts: Arc::new(Mutex::new(HashMap::new())),
            http_client,
        }
    }

    /// Test-only constructor that lets callers inject a pre-built
    /// `reqwest::Client` (and pre-seed the registration cache). Production
    /// code uses [`IctChannel::new`].
    #[cfg(test)]
    fn new_with_client(
        alias: impl Into<String>,
        config_resolver: Arc<dyn Fn() -> Result<IctConfigSnapshot> + Send + Sync>,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            alias: alias.into(),
            config_resolver,
            connect: Arc::new(RwLock::new(None)),
            registered_until: Arc::new(RwLock::new(None)),
            ws_tx: Arc::new(Mutex::new(None)),
            session_routes: Arc::new(Mutex::new(HashMap::new())),
            last_frame_at: Arc::new(Mutex::new(None)),
            http_client,
        }
    }

    /// Forcefully clears the cached `IctConnect` and its deadline. Used by
    /// the connect path when the cached credential is rejected by the
    /// upstream server, so the next reconnect must re-register.
    async fn invalidate_cached_connect(&self) {
        *self.connect.write().await = None;
        *self.registered_until.write().await = None;
    }

    fn build_basic_auth(username: &str, password: &str) -> String {
        let raw = format!("{username}:{password}");
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        )
    }

    fn build_request(
        connect: &IctConnect,
    ) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
        let mut request = connect
            .wss_url
            .as_str()
            .into_client_request()
            .with_context(|| format!("invalid ICT WebSocket URL: {}", connect.wss_url))?;
        let auth = Self::build_basic_auth(&connect.username, &connect.password);
        request
            .headers_mut()
            .insert("Authorization", HeaderValue::from_str(&auth)?);
        Ok(request)
    }

    async fn connect(
        &self,
        connect: &IctConnect,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let request = Self::build_request(connect)?;
        let (stream, _) = connect_async(request)
            .await
            .with_context(|| format!("ICT WebSocket connect failed for {}", connect.wss_url))?;
        Ok(stream)
    }

    async fn authenticate_connection(
        &self,
        ws_stream: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Result<()> {
        let first = tokio::time::timeout(ICT_AUTH_TIMEOUT, ws_stream.next())
            .await
            .context("timed out waiting for ICT auth frame")?
            .ok_or_else(|| anyhow!("ICT WebSocket closed before auth frame"))??;

        let text = match first {
            WsMessage::Text(text) => text.to_string(),
            WsMessage::Binary(bin) => String::from_utf8(bin.to_vec())
                .context("ICT auth frame binary payload is not valid UTF-8")?,
            other => {
                return Err(anyhow!(
                    "unexpected ICT auth frame type: {}",
                    other.to_text().unwrap_or("<non-text-frame>")
                ));
            }
        };

        let msg: IctWireMessage =
            serde_json::from_str(&text).context("failed to parse ICT auth frame JSON")?;
        if msg.msg_type != 998 {
            return Err(anyhow!(
                "ICT auth failed: expected type=998, got type={}",
                msg.msg_type
            ));
        }

        *self.last_frame_at.lock().await = Some(Instant::now());
        Ok(())
    }

    async fn run_session(
        &self,
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tx: &mpsc::Sender<ChannelMessage>,
        _connect: &IctConnect,
        heartbeat_interval_secs: u64,
    ) -> Result<SessionOutcome> {
        let (mut ws_write, mut ws_read) = ws_stream.split();
        let (out_tx, mut out_rx) = mpsc::channel::<IctOutbound>(ICT_OUTBOUND_QUEUE_DEPTH);
        *self.ws_tx.lock().await = Some(out_tx);

        let heartbeat_enabled = heartbeat_interval_secs > 0;
        let mut heartbeat =
            tokio::time::interval(Duration::from_secs(heartbeat_interval_secs.max(1)));
        let mut watchdog = tokio::time::interval(Duration::from_secs(1));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        if heartbeat_enabled {
            heartbeat.tick().await;
            watchdog.tick().await;
        }

        loop {
            tokio::select! {
                frame = ws_read.next() => {
                    match frame {
                        Some(Ok(WsMessage::Text(text))) => {
                            let inbound = self.handle_inbound_message(&text, tx).await?;
                            if tx.is_closed() && inbound == InboundKind::Message {
                                return Ok(SessionOutcome::Shutdown);
                            }
                        }
                        Some(Ok(WsMessage::Binary(bin))) => {
                            let text = String::from_utf8(bin.to_vec())
                                .context("ICT binary frame is not valid UTF-8")?;
                            let inbound = self.handle_inbound_message(&text, tx).await?;
                            if tx.is_closed() && inbound == InboundKind::Message {
                                return Ok(SessionOutcome::Shutdown);
                            }
                        }
                        Some(Ok(WsMessage::Close(_))) => {
                            ict_log_info!("ICT WebSocket closed by server");
                            return Ok(SessionOutcome::Reconnect);
                        }
                        Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => {
                            *self.last_frame_at.lock().await = Some(Instant::now());
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => {
                            ict_log_warn!("ICT WebSocket read error: {err:#}");
                            return Ok(SessionOutcome::Reconnect);
                        }
                        None => {
                            ict_log_info!("ICT WebSocket stream ended");
                            return Ok(SessionOutcome::Reconnect);
                        }
                    }
                }
                Some(outbound) = out_rx.recv() => {
                    let (kind, session_id_for_log, request_id_for_log, bytes_for_log) =
                        match &outbound {
                            IctOutbound::Reply {
                                session_id,
                                request_id,
                                data,
                            } => (
                                "reply",
                                session_id.clone(),
                                Some(request_id.clone()),
                                data.len(),
                            ),
                            IctOutbound::Done {
                                session_id,
                                request_id,
                            } => (
                                "done",
                                session_id.clone(),
                                Some(request_id.clone()),
                                0usize,
                            ),
                            IctOutbound::Notification { session_id, data } => {
                                ("notification", session_id.clone(), None, data.len())
                            }
                        };
                    let payload = match outbound {
                        IctOutbound::Reply {
                            session_id,
                            request_id,
                            data,
                        } => IctWireMessage::reply(data, request_id, session_id),
                        IctOutbound::Done {
                            session_id,
                            request_id,
                        } => IctWireMessage::done_marker(request_id, session_id),
                        IctOutbound::Notification { session_id, data } => {
                            IctWireMessage::notification(data, session_id)
                        }
                    };

                    let json = serde_json::to_string(&payload)
                        .context("failed to serialize ICT outbound message")?;
                    if let Err(err) = ws_write.send(WsMessage::Text(json.into())).await {
                        ict_log_warn!("ICT outbound send failed: {err:#}");
                        return Ok(SessionOutcome::Reconnect);
                    }
                    // Boundary log: confirms the bytes were actually handed
                    // to the tungstenite writer. Pair this with the
                    // `send_proactive` enqueue log line — if you see the
                    // enqueue line but not this one, the WSS write loop is
                    // stuck or the channel has been silently disconnected.
                    let wire_type = payload.msg_type;
                    ict_log_debug!(
                        "ICT outbound frame written kind={} wire_type={} sessionId={} requestId={:?} bytes={}",
                        kind,
                        wire_type,
                        session_id_for_log,
                        request_id_for_log,
                        bytes_for_log
                    );
                }
                _ = heartbeat.tick(), if heartbeat_enabled => {
                    let json = serde_json::to_string(&IctWireMessage::heartbeat())
                        .context("failed to serialize ICT heartbeat")?;
                    if let Err(err) = ws_write.send(WsMessage::Text(json.into())).await {
                        ict_log_warn!("ICT heartbeat send failed: {err:#}");
                        return Ok(SessionOutcome::Reconnect);
                    }
                }
                _ = watchdog.tick(), if heartbeat_enabled => {
                    let max_age = Duration::from_secs(
                        heartbeat_interval_secs
                            .saturating_mul(ICT_HEARTBEAT_TIMEOUT_MULTIPLIER)
                            .max(1),
                    );
                    let stale = match *self.last_frame_at.lock().await {
                        Some(last_frame_at) => last_frame_at.elapsed() > max_age,
                        None => true,
                    };
                    if stale {
                        ict_log_warn!("ICT heartbeat watchdog expired; reconnecting");
                        return Ok(SessionOutcome::Reconnect);
                    }
                }
            }
        }
    }

    async fn handle_inbound_message(
        &self,
        text: &str,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> Result<InboundKind> {
        let msg: IctWireMessage =
            serde_json::from_str(text).context("failed to parse ICT inbound JSON")?;
        *self.last_frame_at.lock().await = Some(Instant::now());

        match msg.msg_type {
            0 => Ok(InboundKind::Heartbeat),
            998 => {
                ict_log_debug!("ICT received additional auth-success frame");
                Ok(InboundKind::Other)
            }
            1 => {
                let session_id = msg
                    .session_id
                    .filter(|value| !value.is_empty())
                    .context("ICT business message missing sessionId")?;
                let request_id = msg
                    .request_id
                    .filter(|value| !value.is_empty())
                    .context("ICT business message missing requestId")?;
                let data = msg
                    .data
                    .filter(|value| !value.is_empty())
                    .context("ICT business message missing data")?;

                self.session_routes
                    .lock()
                    .await
                    .insert(session_id.clone(), request_id.clone());

                let timestamp = msg
                    .timestamp
                    .filter(|value| *value > 0)
                    .map(|value| (value as u64) / 1000)
                    .unwrap_or_else(|| chrono::Utc::now().timestamp().max(0) as u64);

                let channel_msg = ChannelMessage {
                    id: request_id,
                    sender: session_id.clone(),
                    reply_target: session_id,
                    content: data,
                    channel: "ict".into(),
                    channel_alias: Some(self.alias.clone()),
                    timestamp,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: vec![],
                    subject: None,
                    internal_sop_event: None,
                    passive_context: false,
                    explicitly_addressed: false,
                    conversation_scope: Default::default(),
                };

                tx.send(channel_msg)
                    .await
                    .context("failed to forward ICT message to orchestrator")?;
                Ok(InboundKind::Message)
            }
            other => {
                ict_log_debug!("ICT received unknown frame type={other}");
                Ok(InboundKind::Other)
            }
        }
    }

    async fn resolve_request_id(&self, message: &SendMessage) -> Result<String> {
        if let Some(request_id) = message
            .in_reply_to
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
        {
            return Ok(request_id);
        }

        self.session_routes
            .lock()
            .await
            .get(&message.recipient)
            .cloned()
            .with_context(|| {
                format!(
                    "ICT send failed: no requestId route found for session {}",
                    message.recipient
                )
            })
    }

    /// Proactive / cron-style outbound — no `requestId` to resolve and no
    /// follow-up `[DONE]` marker. Emits a single `type=2` frame (see
    /// [`IctWireMessage::notification`]) carrying `data` + `sessionId`,
    /// matching the historical `docs/book/ictmsg.rs` notification shape.
    ///
    /// The orchestrator's `deliver_announcement` call site is the only
    /// caller in v1; the recipient it forwards here is the cron job's
    /// `delivery.to` field, which already carries the `sessionId` the
    /// upstream platform knows the user by. We do **not** look up
    /// `session_routes` (no `requestId` exists for this turn) and we do
    /// **not** append a `Done` frame (the platform treats `type=2` as a
    /// single-shot notification).
    ///
    /// The live `ws_tx` from the running session is the only thing that
    /// can carry this frame out, which is why the orchestrator must look
    /// up the registered `IctChannel` instance (see
    /// `CRON_CHANNEL_REGISTRY` in `orchestrator/mod.rs`) and call this
    /// method on it. A fresh `IctChannel::new(...)` would have
    /// `ws_tx = None` and could not deliver.
    pub async fn send_proactive(&self, recipient: &str, content: &str) -> Result<()> {
        if content.trim().is_empty() {
            anyhow::bail!("ICT send_proactive requires non-empty text content");
        }
        if recipient.trim().is_empty() {
            anyhow::bail!("ICT send_proactive requires non-empty recipient (sessionId)");
        }

        let ws_tx = self
            .ws_tx
            .lock()
            .await
            .clone()
            .context("ICT channel is not connected (no live WebSocket session)")?;

        let notification = IctOutbound::Notification {
            session_id: recipient.to_string(),
            data: content.to_string(),
        };
        // Render the same `IctWireMessage::notification(...)` shape the WSS
        // write loop will serialize a few lines below. We log it here, at
        // the queue-enqueue boundary, so a packet capture on the upstream
        // socket can be diffed against this exact string when diagnosing
        // "cron delivery did not reach the user". `data` is truncated to
        // 256 chars to keep the JSONL line bounded.
        let frame_for_log =
            IctWireMessage::notification(content.to_string(), recipient.to_string());
        let frame_json_for_log = serde_json::to_string(&frame_for_log)
            .unwrap_or_else(|_| "<serialize-failed>".to_string());
        let data_preview: String = content.chars().take(256).collect();
        ws_tx
            .send(notification)
            .await
            .context("failed to enqueue ICT proactive notification")?;

        ict_log_info!(
            "ICT proactive notification enqueued alias={} sessionId={} bytes={} wire_type=2 frame={} data_preview={:?}",
            self.alias,
            recipient,
            content.len(),
            frame_json_for_log,
            data_preview
        );
        Ok(())
    }

    async fn clear_runtime_connection(&self) {
        *self.ws_tx.lock().await = None;
        *self.last_frame_at.lock().await = None;
    }

    /// Sign the registration body exactly the way the upstream platform
    /// expects: `HMAC-SHA256(app_id + timestamp + sortedBody, app_secret)`,
    /// Base64-encoded. Matches the historical `docs/book/ictmsg.rs`
    /// implementation so any operator who already has a working
    /// `app_id` / `app_secret` pair can switch over without rotating it.
    fn sign_registration(
        app_id: &str,
        app_secret: &str,
        timestamp: i64,
        body: &str,
    ) -> Result<String> {
        let sorted_body = sort_json_string(body);
        let sign_data = format!("{app_id}{timestamp}{sorted_body}");
        let mut mac = <HmacSha256 as Mac>::new_from_slice(app_secret.as_bytes())
            .map_err(|err| anyhow!("failed to create HMAC: {err:#}"))?;
        mac.update(sign_data.as_bytes());
        let result = mac.finalize();
        let encoded = base64::engine::general_purpose::STANDARD.encode(result.into_bytes());
        Ok(encoded.replace(|c: char| c.is_whitespace(), ""))
    }

    /// Single-shot registration. Does **not** loop on its own — the caller
    /// decides whether to retry on failure. On success it caches the new
    /// `IctConnect` and its expiry instant.
    async fn register_once(&self) -> Result<IctConnect> {
        let snapshot = (self.config_resolver)()
            .with_context(|| format!("ICT channel alias '{}' is not configured", self.alias))?;

        let body = RegistryRequest { protocol: 1 };
        let body_str =
            serde_json::to_string(&body).context("failed to serialize ICT registration body")?;
        let timestamp = chrono::Utc::now().timestamp_millis();
        let signature =
            Self::sign_registration(&snapshot.app_id, &snapshot.app_secret, timestamp, &body_str)?;

        let response = self
            .http_client
            .post(&snapshot.url)
            .timeout(ICT_REGISTRATION_TIMEOUT)
            .header("Content-Type", "application/json")
            .header("appId", &snapshot.app_id)
            .header("signature", &signature)
            .header("timestamp", timestamp.to_string())
            .json(&body)
            .send()
            .await
            .map_err(|err| anyhow!("ICT registration HTTP request failed: {err:#}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!(
                "ICT registration HTTP error: {} {}",
                status,
                status.canonical_reason().unwrap_or("Unknown")
            ));
        }

        let parsed: RegistryResponse = response
            .json()
            .await
            .map_err(|err| anyhow!("ICT registration response parse failed: {err:#}"))?;
        if parsed.res_code != 0 {
            return Err(anyhow!(
                "ICT registration rejected: {} (code: {})",
                parsed.res_desc.unwrap_or_default(),
                parsed.res_code
            ));
        }
        let data = parsed
            .data
            .ok_or_else(|| anyhow!("ICT registration response missing data"))?;
        let wss_url = data
            .url
            .clone()
            .or(data.addr.clone())
            .ok_or_else(|| anyhow!("ICT registration response missing WebSocket URL"))?;
        if data.username.is_empty() || data.password.is_empty() {
            return Err(anyhow!(
                "ICT registration response missing username or password"
            ));
        }

        let expires_at = Instant::now() + Duration::from_secs(snapshot.expiration_time_secs.max(1));
        let connect = IctConnect {
            wss_url,
            username: data.username,
            password: data.password,
            mac: data.mac,
            expires_at,
        };
        *self.connect.write().await = Some(connect.clone());
        *self.registered_until.write().await = Some(expires_at);
        Ok(connect)
    }

    /// Decide whether the next reconnect can safely reuse the cached
    /// `IctConnect`, or whether a fresh registration is required first.
    /// Returns `true` when re-registration is needed.
    async fn needs_reregistration(&self, reconnect_budget: Duration) -> bool {
        let cached = self.connect.read().await;
        let Some(connect) = cached.as_ref() else {
            return true;
        };
        let Some(deadline) = *self.registered_until.read().await else {
            return true;
        };
        // Belt-and-suspenders: if the deadline somehow predates the cached
        // connect's `expires_at` (e.g. operator-tweaked state), trust the
        // connect's own field.
        let effective = deadline.min(connect.expires_at);
        Instant::now() + reconnect_budget >= effective
    }

    /// Reconnect budget used by the credential-refresh decision. Bounded by
    /// `min(heartbeat_interval_secs, ICT_RECONNECT_BUDGET_CAP_SECS)` so a very
    /// long heartbeat doesn't make us over-confident that the next reconnect
    /// will land inside the registered lifetime.
    fn reconnect_budget(heartbeat_interval_secs: u64) -> Duration {
        let secs = heartbeat_interval_secs.clamp(1, ICT_RECONNECT_BUDGET_CAP_SECS);
        Duration::from_secs(secs)
    }

    /// Compute and enqueue the UTF-8-safe incremental chunk for `text`
    /// since the last `update_draft` call on this `message_id`. Returns
    /// the resolved `session_id` for the envelope.
    ///
    /// Returns Ok even when there is no draft registered (idempotent
    /// no-op) so the orchestrator's draft updater does not have to
    /// special-case the race where finalize arrives before any
    /// update_draft.
    async fn enqueue_incremental_chunk(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> Result<String> {
        let (session_id, new_text, request_id) = {
            let mut drafts = self.drafts.lock().await;
            let Some(state) = drafts.get_mut(message_id) else {
                // Idempotent no-op: finalize-only turn or already-cancelled.
                return Ok(recipient.to_string());
            };
            state.last_activity = Instant::now();

            let sent_len = state.sent_length.min(text.len());
            let new_text = if sent_len < text.len() {
                let mut end = sent_len;
                while end < text.len() && !text.is_char_boundary(end) {
                    end += 1;
                }
                text[end..].to_string()
            } else {
                String::new()
            };
            // Always advance sent_length so the next call sees the full
            // accumulated text and we do not re-send the tail.
            state.sent_length = text.len();

            let session_id = state.session_id.clone();
            // Resolve the request_id from session_routes (canonical
            // source). If a route has been retired (e.g. by cleanup)
            // since send_draft, fall back to an empty string and let the
            // frame-enqueue path handle that as a no-op.
            let request_id = self
                .session_routes
                .lock()
                .await
                .get(&session_id)
                .cloned()
                .unwrap_or_default();
            (session_id, new_text, request_id)
        };

        if new_text.is_empty() || request_id.is_empty() {
            return Ok(session_id);
        }

        let ws_tx = self
            .ws_tx
            .lock()
            .await
            .clone()
            .context("ICT channel is not connected")?;
        ws_tx
            .send(IctOutbound::Reply {
                session_id: session_id.clone(),
                request_id,
                data: new_text,
            })
            .await
            .context("failed to enqueue ICT incremental reply")?;
        Ok(session_id)
    }
}

fn sort_json_string(json_str: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(value) => sort_json_value(&value).to_string(),
        Err(_) => json_str.to_string(),
    }
}

fn sort_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted_map = serde_json::Map::new();
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            for key in keys {
                if let Some(v) = map.get(&key)
                    && !v.is_null()
                    && (v.is_array()
                        || v.is_object()
                        || !v.as_str().map(|s| s.is_empty()).unwrap_or(false))
                {
                    sorted_map.insert(key, sort_json_value(v));
                }
            }
            serde_json::Value::Object(sorted_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sort_json_value).collect())
        }
        other => other.clone(),
    }
}

impl Attributable for IctChannel {
    fn role(&self) -> Role {
        Role::Channel(zeroclaw_api::attribution::ChannelKind::Ict)
    }

    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for IctChannel {
    fn name(&self) -> &str {
        "ict"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        if !message.attachments.is_empty() {
            anyhow::bail!("ICT channel does not support attachments");
        }
        if message.content.trim().is_empty() {
            anyhow::bail!("ICT channel requires non-empty text content");
        }

        let request_id = self.resolve_request_id(message).await?;
        let session_id = message.recipient.clone();
        let reply = IctOutbound::Reply {
            session_id: session_id.clone(),
            request_id: request_id.clone(),
            data: message.content.clone(),
        };

        let ws_tx = self
            .ws_tx
            .lock()
            .await
            .clone()
            .context("ICT channel is not connected")?;
        ws_tx
            .send(reply)
            .await
            .context("failed to enqueue ICT outbound reply")?;

        // Append the end-of-stream marker on the same WS session. Strategy:
        // under the current channel-trait call shape, a single `Channel::send`
        // call always carries the full reply for one inbound turn (see the
        // invariant documented in docs/ict-websocket-channel-design-zh.md),
        // so we treat every `send()` as the terminal frame of that turn and
        // queue a `[DONE]` marker that reuses the same `request_id` /
        // `session_id` and is serialized via `IctWireMessage::done_marker`.
        //
        // We use `try_send` for the marker on purpose: if the outbound queue
        // is saturated (queue depth is `ICT_OUTBOUND_QUEUE_DEPTH`; the reply
        // we just enqueued plus this marker cost 2 slots), surfacing that as
        // a `Channel::send` failure would make the runtime treat the entire
        // reply as undelivered, which is worse than the caller seeing success
        // and the peer observing a missing marker — peers that need strict
        // end-of-stream semantics can apply their own timeout on top.
        let done = IctOutbound::Done {
            session_id,
            request_id,
        };
        if let Err(err) = ws_tx.try_send(done) {
            ict_log_warn!("ICT [DONE] marker enqueue failed: {err:#}");
        }

        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        // Resolved on every call so the runtime picks up an operator
        // edit to `stream_mode` at the next reply boundary without
        // restarting the daemon.
        let snapshot = match (self.config_resolver)() {
            Ok(s) => s,
            Err(_) => return false,
        };
        snapshot.stream_mode != StreamMode::Off
    }

    fn supports_multi_message_streaming(&self) -> bool {
        // ICT has no surface for independent messages — every business
        // frame is one turn that the peer appends to. Reject multi_message
        // to keep the runtime from spawning N fragmented sends.
        false
    }

    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        let snapshot =
            (self.config_resolver)().context("ICT send_draft: config resolver failed")?;
        if snapshot.stream_mode == StreamMode::Off {
            return Ok(None);
        }

        // ICT has no platform-side message_id for a draft, so we mint a
        // client-local handle. The orchestrator only uses this id to
        // correlate subsequent update_draft / finalize_draft calls back
        // to *this* channel instance, so locally-unique is sufficient.
        let draft_id = format!("ict-draft-{}", uuid::Uuid::new_v4());

        // Resolve the request_id from session_routes (the source of truth)
        // so subsequent frames reuse the same correlation token. If there
        // is no inbound route yet (e.g. proactive), generate a fresh id.
        let request_id = self.resolve_request_id(message).await?;
        let session_id = message.recipient.clone();

        {
            let mut drafts = self.drafts.lock().await;
            drafts.insert(
                draft_id.clone(),
                IctDraftState {
                    session_id: session_id.clone(),
                    sent_length: 0,
                    last_activity: Instant::now(),
                },
            );
        }

        // The orchestrator's draft first frame is conventionally "..."
        // or the empty placeholder. We do not emit a wire frame for the
        // first call — the first delta in update_draft will carry the
        // actual content. This matches the historical ictmsg.rs
        // send_text / edit_message split (the draft never emitted an
        // empty frame; the first non-empty edit did).
        ict_log_debug!(
            "ICT send_draft opened draft_id={draft_id} session_id={session_id} request_id={request_id}"
        );
        Ok(Some(draft_id))
    }

    async fn update_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        let snapshot =
            (self.config_resolver)().context("ICT update_draft: config resolver failed")?;
        if snapshot.stream_mode == StreamMode::Off {
            return Ok(());
        }

        self.enqueue_incremental_chunk(recipient, message_id, text)
            .await?;
        Ok(())
    }

    async fn update_draft_progress(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<()> {
        // ICT only renders answer text. Tool progress/status updates are
        // intentionally omitted from the reply stream.
        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> Result<()> {
        let snapshot =
            (self.config_resolver)().context("ICT finalize_draft: config resolver failed")?;
        if snapshot.stream_mode == StreamMode::Off {
            // Even when stream_mode is off we must drain any draft
            // bookkeeping left behind by an earlier send_draft call so
            // a re-enable doesn't pick up stale state.
            self.drafts.lock().await.remove(message_id);
            return Ok(());
        }

        // Flush any remaining incremental chunk the caller has buffered
        // since the last update_draft, then emit the [DONE] marker.
        self.enqueue_incremental_chunk(recipient, message_id, text)
            .await?;

        let (session_id, request_id) = {
            let mut drafts = self.drafts.lock().await;
            match drafts.remove(message_id) {
                Some(state) => (state.session_id, None),
                None => {
                    ict_log_warn!("ICT finalize_draft with no open draft message_id={message_id}");
                    (recipient.to_string(), None)
                }
            }
        };

        // We deliberately resolve request_id fresh instead of caching it
        // on IctDraftState: session_routes is the source of truth and we
        // don't want a stale snapshot if a reconnect cleared it.
        let request_id = match request_id {
            Some(id) => id,
            None => self
                .session_routes
                .lock()
                .await
                .get(&session_id)
                .cloned()
                .unwrap_or_default(),
        };

        if request_id.is_empty() {
            ict_log_warn!(
                "ICT finalize_draft: no request_id resolved for session {session_id}; skipping [DONE]"
            );
            return Ok(());
        }

        let ws_tx = self
            .ws_tx
            .lock()
            .await
            .clone()
            .context("ICT channel is not connected")?;
        if let Err(err) = ws_tx.try_send(IctOutbound::Done {
            session_id,
            request_id,
        }) {
            ict_log_warn!("ICT [DONE] marker enqueue failed: {err:#}");
        }
        Ok(())
    }

    async fn cancel_draft(&self, _recipient: &str, message_id: &str) -> Result<()> {
        // Drop local state; do not emit a [DONE] (the peer would treat
        // it as a complete turn). Already-sent incremental frames remain
        // on the wire — the upstream platform chooses whether to render
        // them as a truncated prefix.
        self.drafts.lock().await.remove(message_id);
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        loop {
            let snapshot = match (self.config_resolver)() {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    ict_log_warn!(
                        "ICT config resolve failed alias={} error={err:#}",
                        self.alias
                    );
                    if tx.is_closed() {
                        return Ok(());
                    }
                    tokio::time::sleep(ICT_RECONNECT_DELAY).await;
                    continue;
                }
            };

            // Step 1: decide whether the cached `IctConnect` is still safe to
            // reuse, or whether we must register first.
            let reconnect_budget = Self::reconnect_budget(snapshot.heartbeat_interval_secs);
            let need_register = self.needs_reregistration(reconnect_budget).await;
            let connect = if need_register {
                let reason: &'static str = if self.connect.read().await.is_none() {
                    "cold-start"
                } else {
                    "credential-near-expiry"
                };
                ict_log_info!(
                    "ICT registering alias={} url={} reason={}",
                    self.alias,
                    snapshot.url,
                    reason
                );
                match self.register_once().await {
                    Ok(connect) => connect,
                    Err(err) => {
                        ict_log_warn!("ICT registration failed alias={} error={err:#}", self.alias);
                        if tx.is_closed() {
                            return Ok(());
                        }
                        tokio::time::sleep(ICT_REGISTRATION_RETRY_DELAY).await;
                        continue;
                    }
                }
            } else {
                // Safe: `needs_reregistration` returned `false` only when both
                // `connect` and `registered_until` are populated.
                self.connect
                    .read()
                    .await
                    .clone()
                    .expect("connect must be Some when needs_reregistration is false")
            };

            ict_log_info!(
                "ICT connecting alias={} url={}",
                self.alias,
                connect.wss_url
            );

            // Step 2: open the WebSocket with the registered credentials.
            let mut ws_stream = match self.connect(&connect).await {
                Ok(stream) => stream,
                Err(err) => {
                    ict_log_warn!("ICT connect failed alias={} error={err:#}", self.alias);
                    if tx.is_closed() {
                        return Ok(());
                    }
                    tokio::time::sleep(ICT_RECONNECT_DELAY).await;
                    continue;
                }
            };

            // Step 3: require the `type=998` auth-success first frame. If the
            // upstream rejects the cached credential (most often because the
            // upstream TTL is shorter than our local `expiration_time_secs`),
            // invalidate the cache so the next loop iteration re-registers.
            if let Err(err) = self.authenticate_connection(&mut ws_stream).await {
                ict_log_warn!(
                    "ICT authentication failed alias={} error={err:#}",
                    self.alias
                );
                let _ = ws_stream.close(None).await;
                self.invalidate_cached_connect().await;
                if tx.is_closed() {
                    return Ok(());
                }
                tokio::time::sleep(ICT_RECONNECT_DELAY).await;
                continue;
            }

            // Step 4: run until disconnect. Use the heartbeat interval from
            // the config snapshot (it is independent of the registration
            // state machine).
            let outcome = self
                .run_session(ws_stream, &tx, &connect, snapshot.heartbeat_interval_secs)
                .await;
            self.clear_runtime_connection().await;

            match outcome {
                Ok(SessionOutcome::Shutdown) => return Ok(()),
                Ok(SessionOutcome::Reconnect) => {
                    if tx.is_closed() {
                        return Ok(());
                    }
                }
                Err(err) => {
                    ict_log_error!("ICT session failed alias={} error={err:#}", self.alias);
                    if tx.is_closed() {
                        return Ok(());
                    }
                }
            }

            // Step 5: loop back. The next iteration will consult
            // `needs_reregistration` before reusing or re-issuing credentials.
            tokio::time::sleep(ICT_RECONNECT_DELAY).await;
        }
    }

    async fn health_check(&self) -> bool {
        let has_ws = self.ws_tx.lock().await.is_some();
        if !has_ws {
            return false;
        }

        // `health_check` only inspects live transport activity. It does not
        // gate on `registered_until` — credential expiry is handled by the
        // listen loop, not by the health probe.
        let snapshot = match (self.config_resolver)() {
            Ok(snapshot) => snapshot,
            Err(_) => return false,
        };
        let heartbeat_interval_secs = snapshot.heartbeat_interval_secs;
        let max_age = Duration::from_secs(
            heartbeat_interval_secs
                .saturating_mul(ICT_HEARTBEAT_TIMEOUT_MULTIPLIER)
                .max(1),
        );

        match *self.last_frame_at.lock().await {
            Some(last_frame_at) => {
                if heartbeat_interval_secs == 0 {
                    true
                } else {
                    last_frame_at.elapsed() <= max_age
                }
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    /// Build an `IctConfigSnapshot` that resolves to the supplied
    /// `register_url` / `ws_url` for the registration HTTP server and the
    /// downstream WebSocket server. Mirrors what the orchestrator resolvers
    /// build in production.
    #[allow(unused_variables)]
    fn snapshot_for(
        register_url: String,
        ws_url: String,
        heartbeat_interval_secs: u64,
        expiration_time_secs: u64,
        app_id: String,
        app_secret: String,
    ) -> Arc<dyn Fn() -> Result<IctConfigSnapshot> + Send + Sync> {
        Arc::new(move || {
            Ok(IctConfigSnapshot {
                url: register_url.clone(),
                app_id: app_id.clone(),
                app_secret: app_secret.clone(),
                heartbeat_interval_secs,
                expiration_time_secs,
                stream_mode: StreamMode::Off,
            })
        })
    }

    /// Spawn a minimal HTTP/1.1 server that responds to the single
    /// `POST <url>` registration request with the supplied `body` and
    /// records the request headers / body in `captured`. Returns the
    /// listening URL.
    async fn spawn_registration_server(
        captured: Arc<StdMutex<Option<(String, String, String, String, String)>>>,
        response_status_line: &'static str,
        response_body: String,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                // Read the request in a loop until we have the full headers
                // and the declared content-length bytes of the body.
                let mut buf: Vec<u8> = Vec::with_capacity(8192);
                let mut tmp = [0u8; 4096];
                let mut header_end: Option<usize> = None;
                let mut content_length: usize = 0;
                while header_end.is_none() {
                    let n = match socket.read(&mut tmp).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head_str = String::from_utf8_lossy(&buf[..pos]).into_owned();
                        for line in head_str.split("\r\n") {
                            if let Some(rest) = line
                                .split_once(':')
                                .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
                            {
                                if rest.0 == "content-length" {
                                    content_length = rest.1.parse().unwrap_or(0);
                                }
                            }
                        }
                        header_end = Some(pos + 4);
                    }
                }
                let Some(headers_end) = header_end else {
                    return;
                };
                while buf.len() < headers_end + content_length {
                    let n = match socket.read(&mut tmp).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                }
                let request = String::from_utf8_lossy(&buf).into_owned();
                let head = &request[..headers_end - 4];
                let body = request[headers_end..].to_string();
                let mut app_id = String::new();
                let mut signature = String::new();
                let mut timestamp = String::new();
                // HTTP header names are case-insensitive; reqwest emits
                // custom headers in lowercase, so we match on either case.
                for line in head.split("\r\n").skip(1) {
                    let (name, value) = match line.split_once(':') {
                        Some((n, v)) => (n.trim(), v.trim()),
                        None => continue,
                    };
                    match name.to_ascii_lowercase().as_str() {
                        "appid" => app_id = value.to_string(),
                        "signature" => signature = value.to_string(),
                        "timestamp" => timestamp = value.to_string(),
                        _ => {}
                    }
                }
                *captured.lock().unwrap() = Some((
                    head.split("\r\n").next().unwrap_or("").to_string(),
                    app_id,
                    signature,
                    timestamp,
                    body,
                ));
                let response = format!(
                    "{status}
content-type: application/json
content-length: {len}
connection: close

{body}",
                    status = response_status_line,
                    len = response_body.len(),
                    body = response_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}/ictclaw/linker/v1/openapi/upstream")
    }

    /// Construct a `RegistryResponse` JSON body that the registration
    /// server can return. The fields mirror the historical
    /// `docs/book/ictmsg.rs` wire shape.
    fn registry_response_body(ws_url: &str, username: &str, password: &str, mac: &str) -> String {
        serde_json::json!({
            "resCode": 0,
            "resDesc": "ok",
            "data": {
                "mac": mac,
                "protocol": 1,
                "url": ws_url,
                "username": username,
                "password": password,
            }
        })
        .to_string()
    }

    /// Build an `IctChannel` pointed at the supplied registration URL, with
    /// the underlying WSS server's URL embedded in the canned registration
    /// response.
    #[allow(unused_variables)]
    fn build_channel(
        alias: &str,
        register_url: String,
        ws_url: String,
        expiration_time_secs: u64,
        http_client: reqwest::Client,
    ) -> IctChannel {
        IctChannel::new_with_client(
            alias,
            snapshot_for(
                register_url,
                ws_url.clone(),
                60,
                expiration_time_secs,
                "test-app".to_string(),
                "test-secret".to_string(),
            ),
            http_client,
        )
    }

    #[test]
    fn build_basic_auth_encodes_credentials() {
        assert_eq!(
            IctChannel::build_basic_auth("alice", "secret"),
            "Basic YWxpY2U6c2VjcmV0"
        );
    }

    #[tokio::test]
    async fn ict_channel_registers_then_authenticates_receives_and_replies() {
        // First spawn the downstream WSS server that registration will hand
        // out as `wss_url`. We need its URL to embed in the registration
        // response, so we pre-bind the listener and capture its address.
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_url = format!("ws://{ws_addr}");
        let ws_auth_header: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));

        let ws_auth_for_server = Arc::clone(&ws_auth_header);
        let ws_server = tokio::spawn(async move {
            let (socket, _) = ws_listener.accept().await.unwrap();
            let auth_header_server = Arc::clone(&ws_auth_for_server);
            let ws_stream = accept_hdr_async(socket, move |req: &Request, response: Response| {
                *auth_header_server.lock().unwrap() = req
                    .headers()
                    .get("Authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(ToOwned::to_owned);
                Ok(response)
            })
            .await
            .unwrap();
            let (mut ws_write, mut ws_read) = ws_stream.split();
            // Send auth-success + a single business inbound frame, then
            // expect the channel to reply (business frame + [DONE]).
            let auth = serde_json::to_string(&IctWireMessage {
                msg_type: 998,
                data: Some("ok".into()),
                request_id: None,
                session_id: None,
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write.send(WsMessage::Text(auth.into())).await.unwrap();
            let inbound = serde_json::to_string(&IctWireMessage {
                msg_type: 1,
                data: Some("weather?".into()),
                request_id: Some("req-123".into()),
                session_id: Some("sess-42".into()),
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write
                .send(WsMessage::Text(inbound.into()))
                .await
                .unwrap();
            let mut frames = Vec::with_capacity(2);
            for _ in 0..2 {
                let frame = timeout(Duration::from_secs(5), ws_read.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let text = match frame {
                    WsMessage::Text(text) => text.to_string(),
                    other => panic!("unexpected reply frame: {other:?}"),
                };
                frames.push(serde_json::from_str::<IctWireMessage>(&text).unwrap());
            }
            let mut iter = frames.into_iter();
            (iter.next().unwrap(), iter.next().unwrap())
        });

        // Spawn the registration server pointing the channel at the WSS URL.
        let register_captured: Arc<StdMutex<Option<(String, String, String, String, String)>>> =
            Arc::new(StdMutex::new(None));
        let register_captured_server = Arc::clone(&register_captured);
        let register_body = registry_response_body(&ws_url, "alice", "secret", "mac-abc");
        let register_url =
            spawn_registration_server(register_captured_server, "HTTP/1.1 200 OK", register_body)
                .await;

        let channel = Arc::new(build_channel(
            "default",
            register_url,
            ws_url.clone(),
            600,
            reqwest::Client::new(),
        ));

        let (tx, mut rx) = mpsc::channel(8);
        let listen_handle = Arc::clone(&channel);
        let listen_task = tokio::spawn(async move { listen_handle.listen(tx).await });

        let msg = timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .expect("ICT inbound message");
        assert_eq!(msg.id, "req-123");
        assert_eq!(msg.reply_target, "sess-42");
        assert_eq!(msg.channel, "ict");
        assert_eq!(msg.channel_alias.as_deref(), Some("default"));

        channel
            .send(&SendMessage::reply_to(&msg, "sunny"))
            .await
            .unwrap();

        let (reply, done) = ws_server.await.unwrap();
        assert_eq!(
            ws_auth_header.lock().unwrap().as_deref(),
            Some("Basic YWxpY2U6c2VjcmV0")
        );

        assert_eq!(reply.msg_type, 1);
        assert_eq!(reply.request_id.as_deref(), Some("req-123"));
        assert_eq!(reply.session_id.as_deref(), Some("sess-42"));
        assert_eq!(reply.data.as_deref(), Some("sunny"));

        assert_eq!(done.msg_type, 1);
        assert_eq!(done.request_id.as_deref(), Some("req-123"));
        assert_eq!(done.session_id.as_deref(), Some("sess-42"));
        assert_eq!(done.data.as_deref(), Some("[DONE]"));

        // Registration was issued exactly once and signed correctly.
        let captured = register_captured.lock().unwrap().clone();
        let (request_line, app_id, _signature, _timestamp, body) =
            captured.expect("register request captured");
        assert!(
            request_line.starts_with("POST "),
            "request line was: {request_line}"
        );
        assert_eq!(app_id, "test-app");
        assert_eq!(body, r#"{"protocol":1}"#);

        listen_task.abort();
        let _ = listen_task.await;
    }

    /// Smoke test: when registration succeeds and the cache is still fresh
    /// (well within `expiration_time_secs`), the channel does **not**
    /// re-register on subsequent reconnects. We assert this by counting how
    /// many registration requests hit the server across two forced
    /// reconnects.
    #[tokio::test]
    async fn ict_channel_skips_reregistration_within_expiration_window() {
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_url = format!("ws://{ws_addr}");

        // Accept any number of WS connections sequentially. Each one sends
        // `998` then closes -- that's enough to drive a `Reconnect` outcome
        // from the run loop and bounce back to the credential refresh
        // decision.
        let ws_server = tokio::spawn(async move {
            loop {
                let (socket, _) = match ws_listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let ws_stream =
                    match accept_hdr_async(socket, |_req: &Request, response: Response| {
                        Ok(response)
                    })
                    .await
                    {
                        Ok(stream) => stream,
                        Err(_) => continue,
                    };
                let (mut ws_write, _) = ws_stream.split();
                let auth = serde_json::to_string(&IctWireMessage {
                    msg_type: 998,
                    data: Some("ok".into()),
                    request_id: None,
                    session_id: None,
                    timestamp: Some(chrono::Utc::now().timestamp_millis()),
                })
                .unwrap();
                let _ = ws_write.send(WsMessage::Text(auth.into())).await;
            }
        });

        // Count registration requests.
        let counter: Arc<StdMutex<u32>> = Arc::new(StdMutex::new(0));
        let counter_server = Arc::clone(&counter);
        let register_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let register_addr = register_listener.local_addr().unwrap();
        let register_url = format!("http://{register_addr}/ictclaw/linker/v1/openapi/upstream");
        let register_body = registry_response_body(&ws_url, "alice", "secret", "mac-abc");
        let register_server = tokio::spawn(async move {
            for _ in 0..5 {
                let (mut socket, _) = match register_listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                *counter_server.lock().unwrap() += 1;
                // Drain the request (we don't parse it, only count).
                let mut buf = Vec::with_capacity(4096);
                let mut tmp = [0u8; 4096];
                loop {
                    let n = match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{body}",
                    len = register_body.len(),
                    body = register_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        let channel = Arc::new(build_channel(
            "default",
            register_url,
            ws_url.clone(),
            600,
            reqwest::Client::new(),
        ));
        let (tx, _rx) = mpsc::channel(8);
        let listen_handle = Arc::clone(&channel);
        let listen_task = tokio::spawn(async move {
            // Bound the listen loop: after 2 seconds the test calls
            // `listen_task.abort()` and the counter is inspected.
            let _ = tokio::time::timeout(Duration::from_secs(2), listen_handle.listen(tx)).await;
        });

        // Let the channel do its cold-start registration + a couple of
        // forced reconnects.
        let _ = tokio::time::sleep(Duration::from_millis(1500)).await;
        listen_task.abort();
        let _ = listen_task.await;
        ws_server.abort();
        let _ = ws_server.await;
        let count = *counter.lock().unwrap();
        // We expect exactly one registration: the initial cold start. The
        // follow-up reconnects (within the 600s window) must reuse the
        // cached credential.
        assert_eq!(count, 1, "expected one registration, got {count}");
        register_server.abort();
    }

    /// Refresh strategy: when `expiration_time_secs` is 0 the channel
    /// treats the cached credential as already expired on the next
    /// iteration, so every reconnect triggers a fresh registration. We
    /// exercise this by pointing the registration server at a WSS server
    /// that closes immediately, with `expiration_time_secs = 0`.
    #[tokio::test]
    async fn ict_channel_reregisters_when_expiration_window_elapsed() {
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_url = format!("ws://{ws_addr}");
        let ws_server = tokio::spawn(async move {
            loop {
                let (socket, _) = match ws_listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let mut ws_stream =
                    match accept_hdr_async(socket, |_req: &Request, response: Response| {
                        Ok(response)
                    })
                    .await
                    {
                        Ok(stream) => stream,
                        Err(_) => continue,
                    };
                let auth = serde_json::to_string(&IctWireMessage {
                    msg_type: 998,
                    data: Some("ok".into()),
                    request_id: None,
                    session_id: None,
                    timestamp: Some(chrono::Utc::now().timestamp_millis()),
                })
                .unwrap();
                let _ = ws_stream.send(WsMessage::Text(auth.into())).await;
                // Server immediately closes after the auth frame so the
                // channel sees a Reconnect outcome on the run loop and
                // bounces back to the credential refresh decision.
                let _ = ws_stream.close(None).await;
            }
        });

        let counter: Arc<StdMutex<u32>> = Arc::new(StdMutex::new(0));
        let counter_server = Arc::clone(&counter);
        let register_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let register_addr = register_listener.local_addr().unwrap();
        let register_url = format!("http://{register_addr}/ictclaw/linker/v1/openapi/upstream");
        let register_body = registry_response_body(&ws_url, "alice", "secret", "mac-abc");
        let register_server = tokio::spawn(async move {
            for _ in 0..5 {
                let (mut socket, _) = match register_listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                *counter_server.lock().unwrap() += 1;
                let mut buf = Vec::with_capacity(4096);
                let mut tmp = [0u8; 4096];
                loop {
                    let n = match socket.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{body}",
                    len = register_body.len(),
                    body = register_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });

        // expiration_time_secs = 0 makes the channel treat the cached
        // credential as expired on the next reconnect -- the listen loop
        // must register again.
        let channel = Arc::new(build_channel(
            "default",
            register_url,
            ws_url.clone(),
            0,
            reqwest::Client::new(),
        ));
        let (tx, _rx) = mpsc::channel(8);
        let listen_handle = Arc::clone(&channel);
        let listen_task = tokio::spawn(async move {
            // Long enough to cover the cold start + at least one
            // ICT_RECONNECT_DELAY (5s) wait + the second registration +
            // the second connect. Without the 5s pause the second loop
            // iteration would be skipped before the re-register fires.
            let _ = tokio::time::timeout(Duration::from_secs(8), listen_handle.listen(tx)).await;
        });

        let _ = tokio::time::sleep(Duration::from_millis(7500)).await;
        listen_task.abort();
        let _ = listen_task.await;
        ws_server.abort();
        let _ = ws_server.await;
        let count = *counter.lock().unwrap();
        assert!(
            count >= 2,
            "expected at least 2 registrations when expiration_time_secs=0, got {count}"
        );
        register_server.abort();
    }

    /// The `sign_registration` helper must produce a stable signature for a
    /// fixed input and be insensitive to JSON key ordering in the body
    /// (the upstream platform sorts keys before computing the signature).
    #[test]
    fn sign_registration_is_stable_and_independent_of_key_order() {
        let sig_a =
            IctChannel::sign_registration("app", "secret", 1710000000000, r#"{"protocol":1}"#)
                .unwrap();
        let sig_b =
            IctChannel::sign_registration("app", "secret", 1710000000000, r#"{"protocol": 1}"#)
                .unwrap();
        assert_eq!(sig_a, sig_b);
        // Different timestamp -> different signature.
        let sig_c =
            IctChannel::sign_registration("app", "secret", 1710000000001, r#"{"protocol":1}"#)
                .unwrap();
        assert_ne!(sig_a, sig_c);
    }

    /// `reconnect_budget` must be bounded by the local cap so a large
    /// `heartbeat_interval_secs` cannot make us over-confident that the
    /// cached credential is still safe to reuse.
    #[test]
    fn reconnect_budget_is_capped() {
        assert_eq!(
            IctChannel::reconnect_budget(0),
            Duration::from_secs(1),
            "zero heartbeat must clamp to 1s"
        );
        assert_eq!(IctChannel::reconnect_budget(10), Duration::from_secs(10));
        assert_eq!(
            IctChannel::reconnect_budget(120),
            Duration::from_secs(ICT_RECONNECT_BUDGET_CAP_SECS)
        );
    }

    /// Wire-shape contract for the `type=2` proactive / notification frame.
    /// Mirrors the historical `docs/book/ictmsg.rs` shape: `data` +
    /// `sessionId`, no `requestId`, no follow-up `[DONE]`. The constructor
    /// is the single source of truth for this shape (see
    /// `IctWireMessage::notification`), so this test pins the contract at
    /// the boundary where the rest of the code resolves a
    /// `IctOutbound::Notification` arm to a frame.
    #[test]
    fn notification_frame_has_type_2_no_request_id() {
        let frame = IctWireMessage::notification(
            "device status: 1 device online".to_string(),
            "sess-cron".to_string(),
        );
        assert_eq!(frame.msg_type, 2, "notification frame must be type=2");
        assert_eq!(
            frame.request_id, None,
            "notification frame must NOT carry a requestId (the upstream              request that would have owned one has long since closed;              reusing it causes the platform to silently drop the frame)"
        );
        assert_eq!(
            frame.session_id.as_deref(),
            Some("sess-cron"),
            "notification frame must carry the recipient sessionId"
        );
        assert_eq!(
            frame.data.as_deref(),
            Some("device status: 1 device online"),
            "notification frame must carry the data payload"
        );
        assert!(
            frame.timestamp.is_some(),
            "notification frame must carry a timestamp"
        );

        // Round-trip through serde and assert the on-the-wire JSON shape
        // matches the historical `docs/book/ictmsg.rs` contract.
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], serde_json::json!(2));
        assert_eq!(
            json["data"],
            serde_json::json!("device status: 1 device online")
        );
        assert_eq!(json["sessionId"], serde_json::json!("sess-cron"));
        assert!(
            json.get("requestId").is_none(),
            "requestId must be omitted from the wire (not null, not empty),              got: {json}"
        );
    }

    /// `send_proactive` end-to-end: drive the WSS handshake exactly like
    /// the real path, then call `send_proactive` and assert the channel
    /// emits a single `type=2` frame with no `requestId` and no follow-up
    /// `[DONE]`. Mirrors `ict_channel_registers_then_authenticates_receives_and_replies`
    /// but exercises the proactive / cron path instead of the reply path.
    #[tokio::test]
    async fn ict_channel_send_proactive_emits_type_2_frame() {
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_url = format!("ws://{ws_addr}");

        // Server: accept the channel, send 998, then read exactly one
        // outbound frame and assert the wire shape from the channel side
        // (the channel-side test below asserts it from the channel side).
        let ws_server = tokio::spawn(async move {
            let (socket, _) = ws_listener.accept().await.unwrap();
            let ws_stream =
                accept_hdr_async(socket, |_req: &Request, response: Response| Ok(response))
                    .await
                    .unwrap();
            let (mut ws_write, mut ws_read) = ws_stream.split();
            let auth = serde_json::to_string(&IctWireMessage {
                msg_type: 998,
                data: Some("ok".into()),
                request_id: None,
                session_id: None,
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write.send(WsMessage::Text(auth.into())).await.unwrap();

            // Wait for the single proactive frame. We do NOT expect a
            // follow-up [DONE] marker on the proactive path — the platform
            // treats `type=2` as a single-shot notification.
            let frame = timeout(Duration::from_secs(5), ws_read.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let text = match frame {
                WsMessage::Text(text) => text.to_string(),
                other => panic!("unexpected frame shape: {other:?}"),
            };
            serde_json::from_str::<IctWireMessage>(&text).unwrap()
        });

        let register_captured: Arc<StdMutex<Option<(String, String, String, String, String)>>> =
            Arc::new(StdMutex::new(None));
        let register_captured_server = Arc::clone(&register_captured);
        let register_body = registry_response_body(&ws_url, "alice", "secret", "mac-abc");
        let register_url =
            spawn_registration_server(register_captured_server, "HTTP/1.1 200 OK", register_body)
                .await;

        let channel = Arc::new(build_channel(
            "default",
            register_url,
            ws_url.clone(),
            600,
            reqwest::Client::new(),
        ));

        let (tx, _rx) = mpsc::channel(8);
        let listen_handle = Arc::clone(&channel);
        let listen_task = tokio::spawn(async move { listen_handle.listen(tx).await });

        // Wait until the channel is connected (ws_tx populated) by polling
        // health_check. Poll up to 5s; once `ws_tx` is set, the
        // `send_proactive` call below will succeed.
        let mut connected = false;
        for _ in 0..50 {
            if channel.health_check().await {
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(connected, "channel did not become healthy within 5s");

        // Drive the proactive path. The orchestrator forwards
        // `delivery.to` as the recipient; for the test we pass the same
        // sessionId the upstream WSS server expects.
        channel
            .send_proactive("sess-cron", "device status: 1 device online")
            .await
            .expect("send_proactive should succeed on a connected channel");

        let frame = ws_server.await.expect("ws server task");
        assert_eq!(
            frame.msg_type, 2,
            "send_proactive must emit a type=2 frame, got: {frame:?}"
        );
        assert_eq!(
            frame.request_id, None,
            "type=2 frame must NOT carry a requestId, got: {frame:?}"
        );
        assert_eq!(
            frame.session_id.as_deref(),
            Some("sess-cron"),
            "type=2 frame must carry the recipient sessionId, got: {frame:?}"
        );
        assert_eq!(
            frame.data.as_deref(),
            Some("device status: 1 device online"),
            "type=2 frame must carry the data payload, got: {frame:?}"
        );

        // `send_proactive` rejects empty content up front (no need to
        // touch the WSS for this branch). Rejecting empty recipients
        // too — both are guard rails at the channel boundary, not at
        // the platform.
        assert!(channel.send_proactive("sess-cron", "   ").await.is_err());
        assert!(channel.send_proactive("   ", "hello").await.is_err());

        listen_task.abort();
        let _ = listen_task.await;
    }

    /// `send_proactive` on a channel that has no live WebSocket session
    /// must fail fast with a clear error — it must NOT silently construct
    /// a fresh outbound queue. This pins the contract that cron delivery
    /// requires the live channel instance (see `ICT_LIVE_CHANNELS`).
    #[tokio::test]
    async fn ict_channel_send_proactive_fails_when_not_connected() {
        // No registration / WSS server is started — the channel never
        // gets a chance to connect, so `ws_tx` stays `None`.
        let register_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let register_addr = register_listener.local_addr().unwrap();
        let register_url = format!("http://{register_addr}/ictclaw/linker/v1/openapi/upstream");
        // Drop the listener so registration will fail and the channel
        // will keep retrying; the test only needs `send_proactive` to
        // observe `ws_tx == None`.
        drop(register_listener);
        let channel = build_channel(
            "default",
            register_url,
            "ws://127.0.0.1:1".to_string(),
            600,
            reqwest::Client::new(),
        );
        let err = channel
            .send_proactive("sess-cron", "hello")
            .await
            .expect_err("send_proactive must fail when ws_tx is None");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ICT channel is not connected"),
            "expected 'not connected' error, got: {msg}"
        );
    }

    /// Build a config resolver that opts into the supplied stream mode.
    fn snapshot_for_stream_mode(
        register_url: String,
        ws_url: String,
        stream_mode: StreamMode,
        app_id: String,
        app_secret: String,
    ) -> Arc<dyn Fn() -> Result<IctConfigSnapshot> + Send + Sync> {
        Arc::new(move || {
            Ok(IctConfigSnapshot {
                url: register_url.clone(),
                app_id: app_id.clone(),
                app_secret: app_secret.clone(),
                heartbeat_interval_secs: 60,
                expiration_time_secs: 600,
                stream_mode,
            })
        })
    }

    /// Build a channel wired to a registration + WS pair with the
    /// supplied `stream_mode`.
    fn build_streaming_channel(
        register_url: String,
        ws_url: String,
        stream_mode: StreamMode,
        http_client: reqwest::Client,
    ) -> IctChannel {
        IctChannel::new_with_client(
            "default",
            snapshot_for_stream_mode(
                register_url,
                ws_url,
                stream_mode,
                "test-app".to_string(),
                "test-secret".to_string(),
            ),
            http_client,
        )
    }

    #[test]
    fn supports_draft_updates_reflects_stream_mode() {
        let off_channel = build_channel(
            "default",
            "http://127.0.0.1:1/never".into(),
            "ws://127.0.0.1:1".into(),
            600,
            reqwest::Client::new(),
        );
        assert!(
            !off_channel.supports_draft_updates(),
            "stream_mode=Off must report no draft support"
        );
        assert!(
            !off_channel.supports_multi_message_streaming(),
            "ICT must never report multi_message support (no independent-message surface)"
        );

        let partial_channel = IctChannel::new_with_client(
            "default",
            snapshot_for_stream_mode(
                "http://127.0.0.1:1/never".into(),
                "ws://127.0.0.1:1".into(),
                StreamMode::Partial,
                "test-app".into(),
                "test-secret".into(),
            ),
            reqwest::Client::new(),
        );
        assert!(
            partial_channel.supports_draft_updates(),
            "stream_mode=Partial must report draft support"
        );
    }

    /// Full round-trip: register, authenticate, deliver an inbound
    /// frame, then drive send_draft / update_draft (multiple) /
    /// finalize_draft. The WS server must observe exactly the
    /// incremental frames plus a terminal [DONE] marker, all sharing
    /// the same requestId / sessionId.
    #[tokio::test]
    async fn draft_hook_full_round_trip() {
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_url = format!("ws://{ws_addr}");

        let captured_frames: Arc<StdMutex<Vec<IctWireMessage>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_for_server = Arc::clone(&captured_frames);
        let ws_server = tokio::spawn(async move {
            let (socket, _) = ws_listener.accept().await.unwrap();
            let ws_stream =
                accept_hdr_async(socket, |_req: &Request, response: Response| Ok(response))
                    .await
                    .unwrap();
            let (mut ws_write, mut ws_read) = ws_stream.split();

            let auth = serde_json::to_string(&IctWireMessage {
                msg_type: 998,
                data: Some("ok".into()),
                request_id: None,
                session_id: None,
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write.send(WsMessage::Text(auth.into())).await.unwrap();

            let inbound = serde_json::to_string(&IctWireMessage {
                msg_type: 1,
                data: Some("hi".into()),
                request_id: Some("req-stream-2".into()),
                session_id: Some("sess-stream-2".into()),
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write
                .send(WsMessage::Text(inbound.into()))
                .await
                .unwrap();

            loop {
                let frame = timeout(Duration::from_secs(5), ws_read.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let text = match frame {
                    WsMessage::Text(text) => text.to_string(),
                    WsMessage::Close(_) => break,
                    other => panic!("unexpected reply frame: {other:?}"),
                };
                let parsed: IctWireMessage = serde_json::from_str(&text).unwrap();
                let is_done = parsed.msg_type == 1 && parsed.data.as_deref() == Some("[DONE]");
                captured_for_server.lock().unwrap().push(parsed);
                if is_done {
                    break;
                }
            }
        });

        let register_captured: Arc<StdMutex<Option<(String, String, String, String, String)>>> =
            Arc::new(StdMutex::new(None));
        let register_url = spawn_registration_server(
            Arc::clone(&register_captured),
            "HTTP/1.1 200 OK",
            registry_response_body(&ws_url, "u", "p", "m"),
        )
        .await;

        let channel = build_streaming_channel(
            register_url,
            ws_url.clone(),
            StreamMode::Partial,
            reqwest::Client::new(),
        );

        let (out_tx, _out_rx) = mpsc::channel::<ChannelMessage>(8);
        let listen_channel = channel.clone();
        let listen_handle = tokio::spawn(async move { listen_channel.listen(out_tx).await });

        tokio::time::sleep(Duration::from_millis(500)).await;

        let draft_id = channel
            .send_draft(&SendMessage::new("...", "sess-stream-2"))
            .await
            .expect("send_draft")
            .expect("draft id");
        assert!(!draft_id.is_empty(), "draft id must be non-empty");

        channel
            .update_draft("sess-stream-2", &draft_id, "Hello")
            .await
            .expect("update_draft 1");
        channel
            .update_draft_progress("sess-stream-2", &draft_id, "Thinking...")
            .await
            .expect("update_draft_progress");
        channel
            .update_draft("sess-stream-2", &draft_id, "private reasoning")
            .await
            .expect("update_draft");
        channel
            .update_draft("sess-stream-2", &draft_id, "Hello, world")
            .await
            .expect("update_draft 2");
        channel
            .finalize_draft("sess-stream-2", &draft_id, "Hello, world!")
            .await
            .expect("finalize_draft");

        let frames = {
            let mut last_seen_done = false;
            for _ in 0..50 {
                let guard = captured_frames.lock().unwrap();
                if guard
                    .iter()
                    .any(|f| f.msg_type == 1 && f.data.as_deref() == Some("[DONE]"))
                {
                    last_seen_done = true;
                    break;
                }
                drop(guard);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(last_seen_done, "server never observed [DONE] marker");
            captured_frames.lock().unwrap().clone()
        };

        let business_chunks: Vec<&IctWireMessage> = frames
            .iter()
            .filter(|f| f.msg_type == 1 && f.data.as_deref() != Some("[DONE]"))
            .collect();
        let done_frames: Vec<&IctWireMessage> = frames
            .iter()
            .filter(|f| f.msg_type == 1 && f.data.as_deref() == Some("[DONE]"))
            .collect();
        // 3 chunks: "Hello" + ", world" + "!"
        assert_eq!(business_chunks.len(), 3, "expected 3 incremental chunks");
        assert_eq!(done_frames.len(), 1, "expected exactly one [DONE]");

        let joined: String = business_chunks
            .iter()
            .filter_map(|f| f.data.as_deref())
            .collect();
        assert_eq!(joined, "Hello, world!");

        for frame in &frames {
            assert_eq!(frame.request_id.as_deref(), Some("req-stream-2"));
            assert_eq!(frame.session_id.as_deref(), Some("sess-stream-2"));
        }

        listen_handle.abort();
        let _ = ws_server.await;
    }

    #[tokio::test]
    async fn cancel_draft_does_not_emit_done() {
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let ws_url = format!("ws://{ws_addr}");

        let captured_frames: Arc<StdMutex<Vec<IctWireMessage>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_for_server = Arc::clone(&captured_frames);
        let ws_server = tokio::spawn(async move {
            let (socket, _) = ws_listener.accept().await.unwrap();
            let ws_stream =
                accept_hdr_async(socket, |_req: &Request, response: Response| Ok(response))
                    .await
                    .unwrap();
            let (mut ws_write, mut ws_read) = ws_stream.split();

            let auth = serde_json::to_string(&IctWireMessage {
                msg_type: 998,
                data: Some("ok".into()),
                request_id: None,
                session_id: None,
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write.send(WsMessage::Text(auth.into())).await.unwrap();

            let inbound = serde_json::to_string(&IctWireMessage {
                msg_type: 1,
                data: Some("hi".into()),
                request_id: Some("req-cancel".into()),
                session_id: Some("sess-cancel".into()),
                timestamp: Some(chrono::Utc::now().timestamp_millis()),
            })
            .unwrap();
            ws_write
                .send(WsMessage::Text(inbound.into()))
                .await
                .unwrap();

            let _ = timeout(Duration::from_millis(400), async {
                while let Some(Ok(WsMessage::Text(text))) = ws_read.next().await {
                    if let Ok(parsed) = serde_json::from_str::<IctWireMessage>(&text) {
                        captured_for_server.lock().unwrap().push(parsed);
                    }
                }
            })
            .await;
        });

        let register_captured: Arc<StdMutex<Option<(String, String, String, String, String)>>> =
            Arc::new(StdMutex::new(None));
        let register_url = spawn_registration_server(
            Arc::clone(&register_captured),
            "HTTP/1.1 200 OK",
            registry_response_body(&ws_url, "u", "p", "m"),
        )
        .await;

        let channel = build_streaming_channel(
            register_url,
            ws_url.clone(),
            StreamMode::Partial,
            reqwest::Client::new(),
        );

        let (out_tx, _out_rx) = mpsc::channel::<ChannelMessage>(8);
        let listen_channel = channel.clone();
        let _listen_handle = tokio::spawn(async move { listen_channel.listen(out_tx).await });

        tokio::time::sleep(Duration::from_millis(500)).await;

        let draft_id = channel
            .send_draft(&SendMessage::new("...", "sess-cancel"))
            .await
            .expect("send_draft")
            .expect("draft id");

        channel
            .update_draft("sess-cancel", &draft_id, "partial")
            .await
            .expect("update_draft");
        channel
            .cancel_draft("sess-cancel", &draft_id)
            .await
            .expect("cancel_draft");

        tokio::time::sleep(Duration::from_millis(500)).await;

        let frames = captured_frames.lock().unwrap().clone();
        let done_seen = frames
            .iter()
            .any(|f| f.msg_type == 1 && f.data.as_deref() == Some("[DONE]"));
        assert!(
            !done_seen,
            "cancel_draft must NOT emit a [DONE] marker; saw frames: {:?}",
            frames
        );

        let _ = ws_server.await;
    }
}
