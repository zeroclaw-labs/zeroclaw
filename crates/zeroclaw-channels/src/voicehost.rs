//! External voice host channel.

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue, header};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use uuid::Uuid;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    ApprovalSource, AttributedApprovalResponse, Channel, ChannelApprovalRequest,
    ChannelApprovalResponse, ChannelMessage, SendMessage, VoiceEvent,
};
use zeroclaw_config::schema::{VoiceHostConfig, ws_connect_with_proxy_headers_and_config};

const OUTBOUND_CAPACITY: usize = 64;
const WRITER_CONTROL_CAPACITY: usize = 40;
const FINAL_TRANSCRIPT_CAPACITY: usize = 32;
const REPLAY_CONTROL_SCAN_LIMIT: usize = 8;
const REPLAY_CONTROL_SCAN_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_PARTIALS_PER_FINAL: usize = 32;
const RECENT_EVENT_ID_CAPACITY: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
const MAX_EVENT_BYTES: usize = MAX_TRANSCRIPT_BYTES + 4 * 1024;
const PARTIAL_FORWARD_INTERVAL: Duration = Duration::from_millis(250);
const PING_INTERVAL_SECS: u64 = 20;
const RECONNECT_DELAYS_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];

fn next_reconnect_delay(
    reconnect_attempt: &mut usize,
    connected_for: Option<Duration>,
) -> Duration {
    if connected_for.is_some_and(|duration| duration >= Duration::from_secs(PING_INTERVAL_SECS)) {
        *reconnect_attempt = 0;
    }
    let delay = RECONNECT_DELAYS_SECS[(*reconnect_attempt).min(RECONNECT_DELAYS_SECS.len() - 1)];
    *reconnect_attempt = reconnect_attempt.saturating_add(1);
    Duration::from_secs(delay)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceHostBackend {
    Native,
    Wyoming,
}

impl VoiceHostBackend {
    fn from_config(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "native" => Ok(Self::Native),
            "wyoming-events-ws" => Ok(Self::Wyoming),
            other => anyhow::bail!(
                "unsupported voice host backend '{other}'; expected native or wyoming-events-ws"
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wyoming => "wyoming-events-ws",
        }
    }
}

fn is_loopback_endpoint(url: &url::Url) -> bool {
    url.host_str().is_some_and(|host| {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

#[derive(Debug, PartialEq, Eq)]
enum InboundAction {
    None,
    FinalTranscript {
        text: String,
        event_id: Option<String>,
    },
    PartialTranscript(String),
    BargeIn,
    Approval {
        request_id: String,
        decision: ChannelApprovalResponse,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BargeInOutcome {
    Continue,
    DispatchClosed,
    RemoteClosed,
}

enum WriterControl {
    Message(Message),
    ReplayAndClose {
        payload: String,
        completed: oneshot::Sender<()>,
    },
}

fn queue_writer_control(sender: &mpsc::Sender<WriterControl>, control: WriterControl) -> bool {
    match sender.try_send(control) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
            false
        }
    }
}

struct PartialForwardBudget {
    remaining: usize,
    capacity: usize,
}

struct RecentEventIds {
    order: VecDeque<String>,
    values: HashSet<String>,
}

impl RecentEventIds {
    fn new() -> Self {
        Self {
            order: VecDeque::with_capacity(RECENT_EVENT_ID_CAPACITY),
            values: HashSet::with_capacity(RECENT_EVENT_ID_CAPACITY),
        }
    }

    fn contains(&self, event_id: &str) -> bool {
        self.values.contains(event_id)
    }

    fn insert(&mut self, event_id: String) {
        if !self.values.insert(event_id.clone()) {
            return;
        }
        self.order.push_back(event_id);
        if self.order.len() > RECENT_EVENT_ID_CAPACITY
            && let Some(expired) = self.order.pop_front()
        {
            self.values.remove(&expired);
        }
    }
}

impl PartialForwardBudget {
    fn new(capacity: usize) -> Self {
        Self {
            remaining: capacity,
            capacity,
        }
    }

    fn admit(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn reset_after_final(&mut self) {
        self.remaining = self.capacity;
    }
}

/// A text-only bridge to an external process that owns the audio pipeline.
pub struct VoiceHostChannel {
    alias: String,
    backend: VoiceHostBackend,
    url: String,
    voice: Option<String>,
    forward_partials: bool,
    proxy_url: Option<String>,
    approval_timeout_secs: u64,
    excluded_tools: Vec<String>,
    headers: HeaderMap,
    outbound: Arc<RwLock<Option<mpsc::Sender<String>>>>,
    control_tx: Arc<RwLock<Option<mpsc::Sender<ChannelMessage>>>>,
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
}

struct PendingApprovalGuard {
    request_id: Option<String>,
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
}

impl PendingApprovalGuard {
    fn new(
        request_id: String,
        pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
    ) -> Self {
        Self {
            request_id: Some(request_id),
            pending_approvals,
        }
    }

    fn disarm(&mut self) {
        self.request_id = None;
    }
}

impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        let Some(request_id) = self.request_id.take() else {
            return;
        };
        let pending_approvals = Arc::clone(&self.pending_approvals);
        zeroclaw_spawn::spawn!(async move {
            pending_approvals.lock().await.remove(&request_id);
        });
    }
}

impl VoiceHostChannel {
    pub fn new(alias: String, config: VoiceHostConfig) -> Result<Self> {
        let parsed_url =
            url::Url::parse(&config.url).context("invalid voice host WebSocket URL")?;
        anyhow::ensure!(
            matches!(parsed_url.scheme(), "ws" | "wss"),
            "voice host URL scheme must be ws or wss"
        );

        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty());
        anyhow::ensure!(
            api_key.is_none() || parsed_url.scheme() == "wss" || is_loopback_endpoint(&parsed_url),
            "voice host bearer credentials require wss:// for non-loopback endpoints"
        );

        let headers = build_auth_headers(api_key)?;
        Ok(Self {
            alias,
            backend: VoiceHostBackend::from_config(&config.backend)?,
            url: config.url,
            voice: config.voice,
            forward_partials: config.forward_partials,
            proxy_url: config.proxy_url,
            approval_timeout_secs: config.approval_timeout_secs,
            excluded_tools: config.excluded_tools,
            headers,
            outbound: Arc::new(RwLock::new(None)),
            control_tx: Arc::new(RwLock::new(None)),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn message_for_action(&self, action: InboundAction) -> Option<ChannelMessage> {
        let (content, passive_context) = match action {
            InboundAction::FinalTranscript { text, .. } => (text, false),
            InboundAction::PartialTranscript(text) => (text, true),
            InboundAction::BargeIn => (String::new(), false),
            InboundAction::None | InboundAction::Approval { .. } => return None,
        };

        Some(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            sender: "voice-user".into(),
            reply_target: self.alias.clone(),
            content,
            channel: "voicehost".into(),
            channel_alias: Some(self.alias.clone()),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            interruption_scope_id: None,
            passive_context,
            explicitly_addressed: true,
            ..Default::default()
        })
    }

    async fn queue_payload(&self, payload: String) -> Result<()> {
        let sender = self
            .outbound
            .read()
            .await
            .clone()
            .context("voice host is not connected")?;
        sender
            .send(payload)
            .await
            .context("voice host connection closed")
    }

    async fn clear_connection_state(&self) {
        *self.outbound.write().await = None;
        self.pending_approvals.lock().await.clear();
    }

    async fn handle_barge_in(
        &self,
        tx: &mpsc::Sender<ChannelMessage>,
        control_tx: Option<&mpsc::Sender<ChannelMessage>>,
        cancel_tx: &mpsc::Sender<Message>,
        pending_control: &mut Option<ChannelMessage>,
    ) -> Result<BargeInOutcome> {
        if let Some(message) = self.message_for_action(InboundAction::BargeIn) {
            if let Some(control_tx) = control_tx {
                match control_tx.try_send(message) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(message)) => {
                        *pending_control = Some(message);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Ok(BargeInOutcome::DispatchClosed);
                    }
                }
            } else {
                match tx.try_send(message) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(message)) => {
                        *pending_control = Some(message);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Ok(BargeInOutcome::DispatchClosed);
                    }
                }
            }
        }

        let cancel = encode_tts_cancel(self.backend)?;
        match cancel_tx.try_send(Message::Text(cancel.into())) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Ok(BargeInOutcome::RemoteClosed);
            }
        }
        Ok(BargeInOutcome::Continue)
    }
}

impl Attributable for VoiceHostChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::VoiceHost)
    }

    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for VoiceHostChannel {
    fn name(&self) -> &str {
        "voicehost"
    }

    fn excluded_tools(&self) -> &[String] {
        &self.excluded_tools
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        anyhow::ensure!(
            !message.suppress_voice,
            "voice host does not support text-only delivery"
        );

        self.queue_payload(encode_reply(
            self.backend,
            &message.content,
            self.voice.as_deref(),
        )?)
        .await
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut reconnect_attempt = 0usize;
        let mut pending_finals = VecDeque::<ChannelMessage>::new();
        let mut pending_control = None::<ChannelMessage>;
        let mut partial_budget = PartialForwardBudget::new(MAX_PARTIALS_PER_FINAL);
        let mut recent_event_ids = RecentEventIds::new();

        loop {
            if tx.is_closed() {
                self.clear_connection_state().await;
                return Ok(());
            }

            let service_key = format!("channel.voicehost.{}", self.alias);
            let connected = tokio::select! {
                result = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    ws_connect_with_proxy_headers_and_config(
                        &self.url,
                        &service_key,
                        self.proxy_url.as_deref(),
                        &self.headers,
                        Some(
                            WebSocketConfig::default()
                                .max_message_size(Some(MAX_EVENT_BYTES))
                                .max_frame_size(Some(MAX_EVENT_BYTES)),
                        ),
                    ),
                ) => match result {
                    Ok(Ok(connection)) => Ok(connection),
                    Ok(Err(_)) => Err("connect_failed"),
                    Err(_) => Err("connect_timeout"),
                },
                _ = tx.closed() => {
                    self.clear_connection_state().await;
                    return Ok(());
                }
            };

            let (socket, _) = match connected {
                Ok(connection) => connection,
                Err(error) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "backend": self.backend.as_str(),
                                "error": error,
                            })),
                        "voice host connection failed"
                    );
                    let delay = next_reconnect_delay(&mut reconnect_attempt, None);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = tx.closed() => return Ok(()),
                    }
                    continue;
                }
            };

            let connected_at = Instant::now();
            let (mut write, mut read) = socket.split();
            let (outbound_tx, mut outbound_rx) = mpsc::channel(OUTBOUND_CAPACITY);
            let (writer_control_tx, mut writer_control_rx) =
                mpsc::channel::<WriterControl>(WRITER_CONTROL_CAPACITY);
            let (cancel_tx, mut cancel_rx) = mpsc::channel::<Message>(1);
            let dispatch_control_tx = self.control_tx.read().await.clone();
            *self.outbound.write().await = Some(outbound_tx);
            let mut writer_task = zeroclaw_spawn::spawn!(async move {
                let mut ping = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
                ping.tick().await;
                loop {
                    let message = tokio::select! {
                        biased;
                        Some(message) = cancel_rx.recv() => message,
                        Some(control) = writer_control_rx.recv() => match control {
                            WriterControl::Message(message) => message,
                            WriterControl::ReplayAndClose { payload, completed } => {
                                tokio::time::timeout(
                                    WRITE_TIMEOUT,
                                    write.send(Message::Text(payload.into())),
                                )
                                .await
                                .context("voice host replay notice write timed out")?
                                .context("voice host replay notice write failed")?;
                                tokio::time::timeout(WRITE_TIMEOUT, write.send(Message::Close(None)))
                                    .await
                                    .context("voice host close write timed out")?
                                    .context("voice host close write failed")?;
                                let _ = completed.send(());
                                return Ok::<(), anyhow::Error>(());
                            }
                        },
                        Some(payload) = outbound_rx.recv() => Message::Text(payload.into()),
                        _ = ping.tick() => Message::Ping(Vec::new().into()),
                    };
                    tokio::time::timeout(WRITE_TIMEOUT, write.send(message))
                        .await
                        .context("voice host WebSocket write timed out")?
                        .context("voice host WebSocket write failed")?;
                }
                #[allow(unreachable_code)]
                Ok::<(), anyhow::Error>(())
            });
            let mut dispatch_closed = false;
            let mut writer_finished = false;
            let mut last_partial_forwarded = None;
            let mut pending_replay = None::<(String, Option<String>)>;
            let mut replay_completion = None::<(oneshot::Receiver<()>, Option<String>)>;
            let mut replay_control_scan_remaining = 0usize;
            let mut replay_control_scan_deadline = None::<Instant>;

            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "alias": self.alias,
                        "backend": self.backend.as_str(),
                    })),
                "voice host connected"
            );

            loop {
                tokio::select! {
                    biased;
                    _ = tx.closed() => {
                        dispatch_closed = true;
                        break;
                    }
                    writer_result = &mut writer_task => {
                        writer_finished = true;
                        if let Ok(Err(error)) = writer_result {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Fail
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "alias": self.alias,
                                    "error": error.to_string(),
                                })),
                                "voice host writer stopped"
                            );
                        }
                        break;
                    }
                    permit = async {
                        dispatch_control_tx
                            .as_ref()
                            .expect("guard requires a production control sender")
                            .reserve()
                            .await
                    }, if pending_control.is_some() && dispatch_control_tx.is_some() => {
                        match permit {
                            Ok(permit) => {
                                if let Some(message) = pending_control.take() {
                                    permit.send(message);
                                }
                            }
                            Err(_) => {
                                dispatch_closed = true;
                                break;
                            }
                        }
                    }
                    permit = tx.reserve(), if pending_control.is_some() && dispatch_control_tx.is_none() => {
                        match permit {
                            Ok(permit) => {
                                if let Some(message) = pending_control.take() {
                                    permit.send(message);
                                }
                            }
                            Err(_) => {
                                dispatch_closed = true;
                                break;
                            }
                        }
                    }
                    permit = tx.reserve(), if !pending_finals.is_empty() => {
                        match permit {
                            Ok(permit) => {
                                if let Some(message) = pending_finals.pop_front() {
                                    permit.send(message);
                                }
                            }
                            Err(_) => {
                                dispatch_closed = true;
                                break;
                            }
                        }
                    }
                    permit = writer_control_tx.reserve(),
                        if pending_replay.is_some() && replay_control_scan_remaining == 0 =>
                    {
                        match permit {
                            Ok(permit) => {
                                let (payload, event_id) = pending_replay
                                    .take()
                                    .expect("guard requires a pending replay notice");
                                replay_control_scan_deadline = None;
                                let (completed_tx, completed_rx) = oneshot::channel();
                                permit.send(WriterControl::ReplayAndClose {
                                    payload,
                                    completed: completed_tx,
                                });
                                replay_completion = Some((completed_rx, event_id));
                            }
                            Err(_) => break,
                        }
                    }
                    completed = async {
                        let (receiver, _) = replay_completion
                            .as_mut()
                            .expect("guard requires replay completion");
                        receiver.await
                    }, if replay_completion.is_some() => {
                        let (_, event_id) = replay_completion
                            .take()
                            .expect("guard requires replay completion");
                        let _ = completed;
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "capacity": FINAL_TRANSCRIPT_CAPACITY,
                                "event_id": event_id,
                            })),
                            "voice host requires final transcript replay after reconnect"
                        );
                        break;
                    }
                    _ = async {
                        let deadline = replay_control_scan_deadline
                            .expect("guard requires a replay control scan deadline");
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    }, if pending_replay.is_some() && replay_control_scan_remaining > 0 => {
                        // Give already-ready interruption controls a bounded chance to be read
                        // before replay closes the connection, even when the writer frees a slot.
                        replay_control_scan_remaining = 0;
                    }
                    incoming = read.next() => {
                        if pending_replay.is_some() && replay_control_scan_remaining > 0 {
                            replay_control_scan_remaining -= 1;
                        }
                        let raw = match incoming {
                            Some(Ok(Message::Text(text))) => text,
                            Some(Ok(Message::Ping(payload))) => {
                                if !queue_writer_control(
                                    &writer_control_tx,
                                    WriterControl::Message(Message::Pong(payload)),
                                ) && pending_replay.is_none()
                                {
                                    break;
                                }
                                continue;
                            }
                            Some(Ok(Message::Pong(_))) => continue,
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(Message::Binary(_))) => {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Reject
                                    ),
                                    "ignored binary voice host event"
                                );
                                continue;
                            }
                            Some(Ok(_)) => continue,
                            Some(Err(error)) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Fail
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({
                                        "alias": self.alias,
                                        "error": error.to_string(),
                                    })),
                                    "voice host receive failed"
                                );
                                break;
                            }
                        };

                        if raw.len() > MAX_EVENT_BYTES {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Reject
                                )
                                .with_attrs(::serde_json::json!({
                                    "alias": self.alias,
                                    "bytes": raw.len(),
                                })),
                                "rejected oversized voice host event"
                            );
                            continue;
                        }

                        let action = parse_inbound(raw.as_str(), self.forward_partials);
                        match action {
                            InboundAction::Approval { request_id, decision } => {
                                if let Some(responder) =
                                    self.pending_approvals.lock().await.remove(&request_id)
                                {
                                    let _ = responder.send(decision);
                                }
                            }
                            InboundAction::BargeIn => {
                                match self
                                    .handle_barge_in(
                                        &tx,
                                        dispatch_control_tx.as_ref(),
                                        &cancel_tx,
                                        &mut pending_control,
                                    )
                                    .await?
                                {
                                    BargeInOutcome::Continue => {}
                                    BargeInOutcome::DispatchClosed => {
                                        dispatch_closed = true;
                                        break;
                                    }
                                    BargeInOutcome::RemoteClosed => break,
                                }
                            }
                            action @ InboundAction::FinalTranscript { .. } => {
                                let event_id = match &action {
                                    InboundAction::FinalTranscript { event_id, .. } => event_id.clone(),
                                    _ => None,
                                };
                                if pending_replay.is_some() || replay_completion.is_some() {
                                    continue;
                                }
                                if event_id
                                    .as_deref()
                                    .is_some_and(|event_id| recent_event_ids.contains(event_id))
                                {
                                    let payload = encode_transcript_ack(
                                        self.backend,
                                        event_id.as_deref(),
                                    )?;
                                    if !queue_writer_control(
                                        &writer_control_tx,
                                        WriterControl::Message(Message::Text(payload.into())),
                                    ) {
                                        break;
                                    }
                                    continue;
                                }
                                let Some(message) = self.message_for_action(action) else {
                                    continue;
                                };
                                let accepted = if pending_finals.is_empty() {
                                    match tx.try_send(message) {
                                        Ok(()) => true,
                                        Err(mpsc::error::TrySendError::Full(message)) => {
                                            pending_finals.push_back(message);
                                            true
                                        }
                                        Err(mpsc::error::TrySendError::Closed(_)) => {
                                            dispatch_closed = true;
                                            break;
                                        }
                                    }
                                } else if pending_finals.len() < FINAL_TRANSCRIPT_CAPACITY {
                                    pending_finals.push_back(message);
                                    true
                                } else {
                                    false
                                };

                                if accepted {
                                    partial_budget.reset_after_final();
                                    if let Some(event_id) = event_id.clone() {
                                        recent_event_ids.insert(event_id);
                                    }
                                    let payload = encode_transcript_ack(
                                        self.backend,
                                        event_id.as_deref(),
                                    )?;
                                    if !queue_writer_control(
                                        &writer_control_tx,
                                        WriterControl::Message(Message::Text(payload.into())),
                                    ) {
                                        break;
                                    }
                                } else {
                                    let payload = encode_transcript_replay_required(
                                        self.backend,
                                        event_id.as_deref(),
                                    )?;
                                    pending_replay = Some((payload, event_id));
                                    replay_control_scan_remaining = REPLAY_CONTROL_SCAN_LIMIT;
                                    replay_control_scan_deadline = Some(
                                        Instant::now() + REPLAY_CONTROL_SCAN_TIMEOUT,
                                    );
                                }
                            }
                            action @ InboundAction::PartialTranscript(_) => {
                                let now = Instant::now();
                                if last_partial_forwarded.is_some_and(|last: Instant| {
                                    now.duration_since(last) < PARTIAL_FORWARD_INTERVAL
                                }) {
                                    continue;
                                }
                                if !partial_budget.admit() {
                                    continue;
                                }
                                if let Some(message) = self.message_for_action(action) {
                                    match tx.try_send(message) {
                                        Ok(()) => last_partial_forwarded = Some(now),
                                        Err(mpsc::error::TrySendError::Full(_)) => {}
                                        Err(mpsc::error::TrySendError::Closed(_)) => {
                                            dispatch_closed = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            InboundAction::None => {
                                ::zeroclaw_log::record!(
                                    DEBUG,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Reject
                                    )
                                    .with_attrs(::serde_json::json!({
                                        "event_type": safe_event_type(raw.as_str()),
                                    })),
                                    "ignored voice host event"
                                );
                            }
                        }
                    }
                }
            }

            if !writer_finished {
                writer_task.abort();
                let _ = writer_task.await;
            }

            self.clear_connection_state().await;
            if dispatch_closed {
                return Ok(());
            }

            let delay = next_reconnect_delay(&mut reconnect_attempt, Some(connected_at.elapsed()));
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = tx.closed() => return Ok(()),
            }
        }
    }

    async fn listen_with_control(
        &self,
        tx: mpsc::Sender<ChannelMessage>,
        control_tx: mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        *self.control_tx.write().await = Some(control_tx);
        let result = self.listen(tx).await;
        *self.control_tx.write().await = None;
        result
    }

    async fn health_check(&self) -> bool {
        self.outbound
            .read()
            .await
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    }

    fn self_handle(&self) -> Option<String> {
        Some("zeroclaw".into())
    }

    fn is_direct_message(&self, _msg: &ChannelMessage) -> bool {
        true
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    async fn request_approval_attributed(
        &self,
        _recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<AttributedApprovalResponse>> {
        let request_id = Uuid::new_v4().to_string();
        let payload = encode_approval_request(&request_id, request)?;
        let (response_tx, response_rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(request_id.clone(), response_tx);
        let mut cleanup =
            PendingApprovalGuard::new(request_id.clone(), Arc::clone(&self.pending_approvals));

        let response =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), async {
                self.queue_payload(payload).await?;
                response_rx
                    .await
                    .context("voice host approval response channel closed")
            })
            .await
            {
                Ok(Ok(decision)) => AttributedApprovalResponse::operator(decision),
                Ok(Err(_)) => AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    ApprovalSource::Unreachable,
                ),
                Err(_) => AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    ApprovalSource::TimedOut,
                ),
            };
        self.pending_approvals.lock().await.remove(&request_id);
        cleanup.disarm();
        Ok(Some(response))
    }
}

fn build_auth_headers(api_key: Option<&str>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    let Some(api_key) = api_key.map(str::trim).filter(|key| !key.is_empty()) else {
        return Ok(headers);
    };

    let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .context("invalid Authorization header value")?;
    headers.insert(header::AUTHORIZATION, value);
    Ok(headers)
}

fn parse_inbound(raw: &str, forward_partials: bool) -> InboundAction {
    if let Ok(event) = serde_json::from_str::<VoiceEvent>(raw) {
        return match event {
            VoiceEvent::SpeechEnd {
                transcript: Some(text),
            } => bounded_transcript(&text)
                .map(|text| InboundAction::FinalTranscript {
                    text,
                    event_id: event_id_from_raw(raw, "/event_id"),
                })
                .unwrap_or(InboundAction::None),
            VoiceEvent::BargeIn => InboundAction::BargeIn,
            VoiceEvent::SpeechStart
            | VoiceEvent::SpeechEnd { .. }
            | VoiceEvent::TtsCancel
            | VoiceEvent::TtsChunk { .. }
            | VoiceEvent::Say { .. } => InboundAction::None,
        };
    }

    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return InboundAction::None;
    };
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return InboundAction::None;
    };

    match event_type {
        "transcript" => wyoming_text(&value)
            .map(|text| InboundAction::FinalTranscript {
                text,
                event_id: event_id_from_value(&value, "/data/event_id")
                    .or_else(|| event_id_from_value(&value, "/data/data/event_id")),
            })
            .unwrap_or(InboundAction::None),
        "transcript-chunk" if forward_partials => wyoming_text(&value)
            .map(InboundAction::PartialTranscript)
            .unwrap_or(InboundAction::None),
        "user-event" => parse_wyoming_user_event(&value),
        _ => InboundAction::None,
    }
}

fn event_id_from_raw(raw: &str, pointer: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| event_id_from_value(&value, pointer))
}

fn event_id_from_value(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn wyoming_text(value: &Value) -> Option<String> {
    value
        .pointer("/data/text")
        .and_then(Value::as_str)
        .and_then(bounded_transcript)
}

fn bounded_transcript(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.len() <= MAX_TRANSCRIPT_BYTES).then(|| text.to_string())
}

fn parse_wyoming_user_event(value: &Value) -> InboundAction {
    match value.pointer("/data/name").and_then(Value::as_str) {
        Some("barge_in") => InboundAction::BargeIn,
        Some("approval_response") => {
            let request_id = value
                .pointer("/data/data/request_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty());
            let decision = value
                .pointer("/data/data/decision")
                .and_then(Value::as_str)
                .and_then(|decision| match decision {
                    "approve" => Some(ChannelApprovalResponse::Approve),
                    "deny" => Some(ChannelApprovalResponse::Deny),
                    "always" => Some(ChannelApprovalResponse::AlwaysApprove),
                    _ => None,
                });
            match (request_id, decision) {
                (Some(request_id), Some(decision)) => InboundAction::Approval {
                    request_id: request_id.to_string(),
                    decision,
                },
                _ => InboundAction::None,
            }
        }
        _ => InboundAction::None,
    }
}

fn safe_event_type(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .map(|kind| kind.chars().take(64).collect())
        .unwrap_or_else(|| "malformed".into())
}

fn encode_reply(backend: VoiceHostBackend, text: &str, voice: Option<&str>) -> Result<String> {
    match backend {
        VoiceHostBackend::Native => Ok(serde_json::to_string(&VoiceEvent::Say {
            text: text.to_string(),
            voice: voice.map(str::to_string),
        })?),
        VoiceHostBackend::Wyoming => Ok(serde_json::to_string(&WyomingEnvelope {
            kind: "synthesize",
            data: WyomingSynthesizeData {
                text,
                voice: voice.map(|name| WyomingVoice { name }),
            },
        })?),
    }
}

fn encode_tts_cancel(backend: VoiceHostBackend) -> Result<String> {
    match backend {
        VoiceHostBackend::Native => Ok(serde_json::to_string(&VoiceEvent::TtsCancel)?),
        VoiceHostBackend::Wyoming => Ok(serde_json::to_string(&WyomingEnvelope {
            kind: "user-event",
            data: WyomingUserEvent {
                name: "tts_cancel",
                data: EmptyData {},
            },
        })?),
    }
}

fn encode_transcript_ack(backend: VoiceHostBackend, event_id: Option<&str>) -> Result<String> {
    match backend {
        VoiceHostBackend::Native => Ok(serde_json::json!({
            "type": "transcript_ack",
            "event_id": event_id,
        })
        .to_string()),
        VoiceHostBackend::Wyoming => Ok(serde_json::to_string(&WyomingEnvelope {
            kind: "user-event",
            data: WyomingUserEvent {
                name: "transcript_ack",
                data: TranscriptAckData { event_id },
            },
        })?),
    }
}

fn encode_transcript_replay_required(
    backend: VoiceHostBackend,
    event_id: Option<&str>,
) -> Result<String> {
    match backend {
        VoiceHostBackend::Native => Ok(serde_json::json!({
            "type": "error",
            "code": "transcript_replay_required",
            "event_id": event_id,
            "retryable": true,
            "reconnect": true,
        })
        .to_string()),
        VoiceHostBackend::Wyoming => Ok(serde_json::to_string(&WyomingEnvelope {
            kind: "user-event",
            data: WyomingUserEvent {
                name: "transcript_replay_required",
                data: TranscriptReplayRequiredData {
                    event_id,
                    retryable: true,
                    reconnect: true,
                },
            },
        })?),
    }
}

fn encode_approval_request(request_id: &str, request: &ChannelApprovalRequest) -> Result<String> {
    Ok(serde_json::to_string(&WyomingEnvelope {
        kind: "user-event",
        data: WyomingUserEvent {
            name: "approval_request",
            data: ApprovalRequestData {
                request_id,
                tool_name: &request.tool_name,
                arguments_summary: &request.arguments_summary,
            },
        },
    })?)
}

#[derive(Serialize)]
struct WyomingEnvelope<T> {
    #[serde(rename = "type")]
    kind: &'static str,
    data: T,
}

#[derive(Serialize)]
struct WyomingSynthesizeData<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<WyomingVoice<'a>>,
}

#[derive(Serialize)]
struct WyomingVoice<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct WyomingUserEvent<'a, T> {
    name: &'a str,
    data: T,
}

#[derive(Serialize)]
struct ApprovalRequestData<'a> {
    request_id: &'a str,
    tool_name: &'a str,
    arguments_summary: &'a str,
}

#[derive(Serialize)]
struct EmptyData {}

#[derive(Serialize)]
struct TranscriptAckData<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<&'a str>,
}

#[derive(Serialize)]
struct TranscriptReplayRequiredData<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    event_id: Option<&'a str>,
    retryable: bool,
    reconnect: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::ChannelApprovalResponse;
    use zeroclaw_config::schema::VoiceHostConfig;

    fn channel(forward_partials: bool) -> VoiceHostChannel {
        VoiceHostChannel::new(
            "office".into(),
            VoiceHostConfig {
                enabled: true,
                url: "ws://127.0.0.1:8765/ws".into(),
                forward_partials,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn unknown_backend_is_rejected() {
        let error = match VoiceHostChannel::new(
            "office".into(),
            VoiceHostConfig {
                enabled: true,
                backend: "wyomign".into(),
                url: "ws://127.0.0.1:8765/ws".into(),
                ..Default::default()
            },
        ) {
            Ok(_) => panic!("unknown voice host backend must fail closed"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("voice host backend"));
        assert!(error.to_string().contains("native"));
        assert!(error.to_string().contains("wyoming"));
    }

    #[test]
    fn wyoming_event_websocket_profile_has_an_unambiguous_backend_name() {
        let backend = VoiceHostBackend::from_config("wyoming-events-ws").unwrap();
        assert_eq!(backend, VoiceHostBackend::Wyoming);
        assert_eq!(backend.as_str(), "wyoming-events-ws");
        assert!(VoiceHostBackend::from_config("wyoming").is_err());
    }

    #[test]
    fn configured_tools_are_excluded_from_voice_turns() {
        let channel = VoiceHostChannel::new(
            "office".into(),
            VoiceHostConfig {
                enabled: true,
                url: "ws://127.0.0.1:8765/ws".into(),
                excluded_tools: vec!["shell".into(), "send_message_to_peer".into()],
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(channel.excluded_tools(), ["shell", "send_message_to_peer"]);
    }

    #[tokio::test]
    async fn suppressed_voice_message_reports_unsupported_text_delivery() {
        let channel = channel(false);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        *channel.outbound.write().await = Some(outbound_tx);

        let error = channel
            .send(&SendMessage::new("internal error", "office").suppress_voice())
            .await
            .expect_err("VoiceHost cannot claim successful text-only delivery");

        assert!(error.to_string().contains("text-only delivery"));
        assert!(outbound_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn barge_in_reaches_local_dispatch_when_remote_cancel_write_fails() {
        let channel = channel(false);
        let (tx, mut rx) = mpsc::channel(1);
        let (cancel_tx, cancel_rx) = mpsc::channel(1);
        drop(cancel_rx);
        let mut pending_control = None;

        assert_eq!(
            channel
                .handle_barge_in(&tx, None, &cancel_tx, &mut pending_control)
                .await
                .unwrap(),
            BargeInOutcome::RemoteClosed
        );
        assert!(rx.recv().await.unwrap().content.is_empty());
    }

    #[test]
    fn reconnect_backoff_survives_flapping_and_resets_after_stable_connection() {
        let mut attempt = 0;
        let flapping = [
            next_reconnect_delay(&mut attempt, Some(Duration::from_secs(1))),
            next_reconnect_delay(&mut attempt, Some(Duration::from_secs(1))),
            next_reconnect_delay(&mut attempt, Some(Duration::from_secs(1))),
        ];
        assert_eq!(
            flapping,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ]
        );
        assert_eq!(
            next_reconnect_delay(&mut attempt, Some(Duration::from_secs(PING_INTERVAL_SECS)),),
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn shutdown_cancels_an_incomplete_websocket_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let server = zeroclaw_spawn::spawn!(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });

        let channel = VoiceHostChannel::new(
            "office".into(),
            VoiceHostConfig {
                enabled: true,
                url: format!("ws:{0}{0}{address}", '/'),
                ..Default::default()
            },
        )
        .unwrap();
        let (inbound_tx, inbound_rx) = mpsc::channel(1);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            channel.listen(inbound_tx).await.unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), accepted_rx)
            .await
            .unwrap()
            .unwrap();

        drop(inbound_rx);
        tokio::time::timeout(Duration::from_secs(1), channel_listener)
            .await
            .expect("listener should observe shutdown during WebSocket handshake")
            .unwrap();
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn native_final_transcript_maps_to_channel_message() {
        let action = parse_inbound(r#"{"type":"speech_end","transcript":"hello world"}"#, false);
        assert_eq!(
            action,
            InboundAction::FinalTranscript {
                text: "hello world".into(),
                event_id: None,
            }
        );

        let message = channel(false).message_for_action(action).unwrap();
        assert_eq!(message.content, "hello world");
        assert_eq!(message.channel, "voicehost");
        assert_eq!(message.channel_alias.as_deref(), Some("office"));
        assert_eq!(message.reply_target, "office");
        assert!(message.explicitly_addressed);
        assert!(!message.passive_context);
    }

    #[test]
    fn wyoming_final_transcript_maps_to_final_action() {
        assert_eq!(
            parse_inbound(
                r#"{"type":"transcript","data":{"text":"hello from wyoming"}}"#,
                false,
            ),
            InboundAction::FinalTranscript {
                text: "hello from wyoming".into(),
                event_id: None,
            }
        );
    }

    #[test]
    fn empty_finals_and_disabled_partials_are_ignored() {
        assert_eq!(
            parse_inbound(r#"{"type":"speech_end","transcript":"  "}"#, false),
            InboundAction::None
        );
        assert_eq!(
            parse_inbound(
                r#"{"type":"transcript-chunk","data":{"text":"hel"}}"#,
                false,
            ),
            InboundAction::None
        );
    }

    #[test]
    fn enabled_partial_is_passive_context() {
        let action = parse_inbound(r#"{"type":"transcript-chunk","data":{"text":"hel"}}"#, true);
        assert_eq!(action, InboundAction::PartialTranscript("hel".into()));
        let message = channel(true).message_for_action(action).unwrap();
        assert!(message.passive_context);
    }

    #[test]
    fn oversized_native_and_wyoming_transcripts_are_ignored() {
        let oversized = "x".repeat(16 * 1024 + 1);
        let native = serde_json::json!({
            "type": "speech_end",
            "transcript": oversized,
        });
        let wyoming = serde_json::json!({
            "type": "transcript",
            "data": { "text": oversized },
        });

        assert!(matches!(
            parse_inbound(&native.to_string(), false),
            InboundAction::None
        ));
        assert!(matches!(
            parse_inbound(&wyoming.to_string(), false),
            InboundAction::None
        ));
    }

    #[tokio::test]
    async fn partial_burst_does_not_delay_barge_in() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for text in ["hel", "hello"] {
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "type": "transcript-chunk",
                            "data": { "text": text },
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            socket
                .send(Message::Text(
                    r#"{"type":"user-event","data":{"name":"barge_in","data":{}}}"#.into(),
                ))
                .await
                .unwrap();
            socket.next().await.unwrap().unwrap().into_text().unwrap()
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    backend: "wyoming-events-ws".into(),
                    url: format!("ws:{0}{0}{address}", '/'),
                    forward_partials: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        let listener_channel = channel.clone();
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel.listen(inbound_tx).await.unwrap();
        });

        let partial = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(partial.passive_context);
        assert_eq!(partial.content, "hel");

        let interrupt = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(interrupt.content.is_empty());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            r#"{"type":"user-event","data":{"name":"tts_cancel","data":{}}}"#
        );

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn full_transcript_queue_does_not_delay_barge_in_control() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    r#"{"type":"speech_end","transcript":"queue pressure"}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(r#"{"type":"barge_in"}"#.into()))
                .await
                .unwrap();
            socket.next().await.unwrap().unwrap().into_text().unwrap()
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        inbound_tx.send(ChannelMessage::default()).await.unwrap();
        let (control_tx, mut control_rx) = mpsc::channel(8);
        let listener_channel = channel.clone();
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel
                .listen_with_control(inbound_tx, control_tx)
                .await
                .unwrap();
        });

        let control = tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(control.reply_target, "office");
        assert!(control.content.is_empty());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            r#"{"type":"tts_cancel"}"#
        );

        let _placeholder = inbound_rx.recv().await.unwrap();
        let transcript = tokio::time::timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("final transcript should remain queued while dispatch is full")
            .unwrap();
        assert_eq!(transcript.content, "queue pressure");

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn full_shared_control_queue_retains_first_barge_in_for_scope() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"type":"barge_in"}"#.into()))
                .await
                .unwrap();
            socket.next().await.unwrap().unwrap().into_text().unwrap()
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (control_tx, mut control_rx) = mpsc::channel(1);
        control_tx
            .send(ChannelMessage {
                reply_target: "other-scope".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel
                .listen_with_control(inbound_tx, control_tx)
                .await
                .unwrap();
        });

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            r#"{"type":"tts_cancel"}"#
        );
        assert_eq!(control_rx.recv().await.unwrap().reply_target, "other-scope");
        let retained = tokio::time::timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("barge-in must be retained while the shared control queue is full")
            .unwrap();
        assert_eq!(retained.reply_target, "office");
        assert!(retained.content.is_empty());

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn pending_barge_in_survives_socket_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (reconnected_tx, reconnected_rx) = oneshot::channel();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(r#"{"type":"barge_in"}"#.into()))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap().into_text().unwrap(),
                r#"{"type":"tts_cancel"}"#
            );
            socket.close(None).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let _socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            reconnected_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (control_tx, mut control_rx) = mpsc::channel(1);
        control_tx
            .send(ChannelMessage {
                reply_target: "other-scope".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel
                .listen_with_control(inbound_tx, control_tx)
                .await
                .unwrap();
        });

        tokio::time::timeout(Duration::from_secs(5), reconnected_rx)
            .await
            .expect("voice host must reconnect after the socket closes")
            .unwrap();
        assert_eq!(control_rx.recv().await.unwrap().reply_target, "other-scope");
        let retained = tokio::time::timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("barge-in must remain pending across the socket reconnect")
            .unwrap();
        assert_eq!(retained.reply_target, "office");
        assert!(retained.content.is_empty());

        channel_listener.abort();
        let _ = channel_listener.await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn final_transcript_overflow_requires_replay_and_closes_the_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for index in 0..=FINAL_TRANSCRIPT_CAPACITY {
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "type": "speech_end",
                            "event_id": format!("final-{index}"),
                            "transcript": format!("queued-{index}"),
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }

            let mut acknowledged = Vec::new();
            let replay = loop {
                let message = socket.next().await.unwrap().unwrap();
                let payload: serde_json::Value =
                    serde_json::from_str(message.to_text().unwrap()).unwrap();
                if payload["type"] == "transcript_ack" {
                    acknowledged.push(payload["event_id"].as_str().unwrap().to_string());
                } else {
                    break payload;
                }
            };
            let closed = matches!(
                tokio::time::timeout(Duration::from_secs(1), socket.next()).await,
                Ok(None) | Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_)))
            );
            (acknowledged, replay, closed)
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        inbound_tx.send(ChannelMessage::default()).await.unwrap();
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel.listen(inbound_tx).await.unwrap();
        });

        let (acknowledged, replay, closed) = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("overflow must produce a replay contract and close")
            .unwrap();
        assert_eq!(acknowledged.len(), FINAL_TRANSCRIPT_CAPACITY);
        assert_eq!(acknowledged.first().map(String::as_str), Some("final-0"));
        assert_eq!(acknowledged.last().map(String::as_str), Some("final-31"));
        assert_eq!(replay["type"], "error");
        assert_eq!(replay["code"], "transcript_replay_required");
        assert_eq!(replay["event_id"], "final-32");
        assert_eq!(replay["retryable"], true);
        assert_eq!(replay["reconnect"], true);
        assert!(
            closed,
            "overflow must terminate the connection after the replay notice"
        );

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn full_writer_control_queue_does_not_block_barge_in_on_final_overflow() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for index in 0..FINAL_TRANSCRIPT_CAPACITY {
                socket
                    .feed(Message::Text(
                        serde_json::json!({
                            "type": "speech_end",
                            "event_id": format!("overflow-{index}"),
                            "transcript": format!("queued-{index}"),
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            for _ in 0..(WRITER_CONTROL_CAPACITY - FINAL_TRANSCRIPT_CAPACITY) {
                socket
                    .feed(Message::Text(
                        serde_json::json!({
                            "type": "speech_end",
                            "event_id": "overflow-0",
                            "transcript": "duplicate accepted final",
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            socket
                .feed(Message::Text(
                    serde_json::json!({
                        "type": "speech_end",
                        "event_id": format!("overflow-{FINAL_TRANSCRIPT_CAPACITY}"),
                        "transcript": "overflow final",
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket.feed(Message::Ping(Vec::new().into())).await.unwrap();
            socket
                .feed(Message::Text(r#"{"type":"barge_in"}"#.into()))
                .await
                .unwrap();
            socket.flush().await.unwrap();

            let mut acknowledgements = 0usize;
            while let Some(message) = socket.next().await {
                let message = message.unwrap();
                if !message.is_text() {
                    continue;
                }
                let payload = message.to_text().unwrap();
                let payload: Value = serde_json::from_str(payload).unwrap();
                match payload["type"].as_str() {
                    Some("transcript_ack") => acknowledgements += 1,
                    Some(kind) => return (kind.to_owned(), acknowledgements),
                    None => panic!("unexpected server event: {payload}"),
                }
            }
            panic!("connection closed before tts_cancel or replay notice");
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        inbound_tx.send(ChannelMessage::default()).await.unwrap();
        let (control_tx, mut control_rx) = mpsc::channel(1);
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel
                .listen_with_control(inbound_tx, control_tx)
                .await
                .unwrap();
        });

        let control = tokio::time::timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("overflow replay admission must not block buffered barge-in")
            .unwrap();
        assert!(control.content.is_empty());
        let (first_control, acknowledgements_before_control) =
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .expect("remote cancellation must precede the replay notice")
                .unwrap();
        assert_eq!(first_control, "tts_cancel");
        assert!(
            acknowledgements_before_control < WRITER_CONTROL_CAPACITY,
            "tts_cancel arrived after the full ACK backlog drained"
        );

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn acknowledged_pending_finals_survive_a_socket_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for index in 0..2 {
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "type": "speech_end",
                            "event_id": format!("retained-{index}"),
                            "transcript": format!("retained transcript {index}"),
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            for index in 0..2 {
                let ack = socket.next().await.unwrap().unwrap();
                let ack: serde_json::Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
                assert_eq!(ack["type"], "transcript_ack");
                assert_eq!(ack["event_id"], format!("retained-{index}"));
            }
            socket.close(None).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let _socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            std::future::pending::<()>().await;
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        inbound_tx.send(ChannelMessage::default()).await.unwrap();
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel.listen(inbound_tx).await.unwrap();
        });

        let _placeholder = inbound_rx.recv().await.unwrap();
        for index in 0..2 {
            let transcript = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
                .await
                .expect("acknowledged final must survive reconnect")
                .unwrap();
            assert_eq!(transcript.content, format!("retained transcript {index}"));
        }

        channel_listener.abort();
        let _ = channel_listener.await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn replayed_event_id_is_acknowledged_without_duplicate_dispatch() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "type": "speech_end",
                            "event_id": "replayed-final",
                            "transcript": "dispatch once",
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let ack = socket.next().await.unwrap().unwrap();
                let ack: Value = serde_json::from_str(ack.to_text().unwrap()).unwrap();
                assert_eq!(ack["type"], "transcript_ack");
                assert_eq!(ack["event_id"], "replayed-final");
                socket.close(None).await.unwrap();
            }
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel.listen(inbound_tx).await.unwrap();
        });

        let transcript = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(transcript.content, "dispatch once");
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(250), inbound_rx.recv())
                .await
                .is_err(),
            "a replayed acknowledged event_id must not dispatch twice"
        );

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn websocket_decoder_rejects_a_padded_oversized_event() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "speech_end",
                        "event_id": "oversized",
                        "transcript": "small",
                        "padding": "x".repeat(MAX_EVENT_BYTES),
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            matches!(
                tokio::time::timeout(Duration::from_secs(2), socket.next()).await,
                Ok(None) | Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_)))
            )
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, mut inbound_rx) = mpsc::channel(2);
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel.listen(inbound_tx).await.unwrap();
        });

        assert!(
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap(),
            "decoder must close before materializing a padded oversized event"
        );
        assert!(inbound_rx.try_recv().is_err());

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[test]
    fn passive_partial_budget_resets_only_after_a_final() {
        let mut budget = PartialForwardBudget::new(MAX_PARTIALS_PER_FINAL);
        for _ in 0..MAX_PARTIALS_PER_FINAL {
            assert!(budget.admit());
        }
        assert!(!budget.admit());
        budget.reset_after_final();
        assert!(budget.admit());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ack_burst_does_not_starve_inbound_barge_in() {
        const ACK_BURST: usize = 20;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ack_count_tx, ack_count_rx) = oneshot::channel();
        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for index in 0..ACK_BURST {
                socket
                    .feed(Message::Text(
                        serde_json::json!({
                            "type": "speech_end",
                            "event_id": format!("final-{index}"),
                            "transcript": format!("accepted-{index}"),
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            socket
                .feed(Message::Text(r#"{"type":"barge_in"}"#.into()))
                .await
                .unwrap();
            socket.flush().await.unwrap();

            let mut acknowledgements_before_cancel = 0;
            while let Some(message) = socket.next().await {
                let message = message.unwrap().into_text().unwrap();
                let kind = serde_json::from_str::<Value>(&message).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                match kind.as_str() {
                    "transcript_ack" => acknowledgements_before_cancel += 1,
                    "tts_cancel" => {
                        ack_count_tx.send(acknowledgements_before_cancel).unwrap();
                        return;
                    }
                    other => panic!("unexpected server event: {other}"),
                }
            }
            panic!("connection closed before tts_cancel");
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    url: format!("ws:{0}{0}{address}", '/'),
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(ACK_BURST);
        let (control_tx, mut control_rx) = mpsc::channel(1);
        let listener_channel = Arc::clone(&channel);
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel
                .listen_with_control(inbound_tx, control_tx)
                .await
                .unwrap();
        });

        let control = tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
            .await
            .expect("barge-in must not wait for the ACK burst to drain")
            .unwrap();
        let acknowledgements_before_cancel =
            tokio::time::timeout(Duration::from_secs(5), ack_count_rx)
                .await
                .expect("remote must receive tts_cancel")
                .unwrap();

        assert!(control.content.is_empty());
        assert!(
            acknowledgements_before_cancel <= 1,
            "tts_cancel arrived after {acknowledgements_before_cancel} ACKs"
        );

        channel_listener.abort();
        let _ = channel_listener.await;
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn ack_capacity_is_owned_by_the_writer_queue() {
        let (writer_control_tx, _writer_control_rx) =
            mpsc::channel::<WriterControl>(WRITER_CONTROL_CAPACITY);

        for _ in 0..WRITER_CONTROL_CAPACITY {
            assert!(queue_writer_control(
                &writer_control_tx,
                WriterControl::Message(Message::Text("ack".into())),
            ));
        }
        assert!(!queue_writer_control(
            &writer_control_tx,
            WriterControl::Message(Message::Text("overflow".into())),
        ));
    }

    #[test]
    fn replies_encode_for_native_and_wyoming_backends() {
        assert_eq!(
            encode_reply(VoiceHostBackend::Native, "hello", Some("en-US")).unwrap(),
            r#"{"type":"say","text":"hello","voice":"en-US"}"#
        );
        assert_eq!(
            encode_reply(VoiceHostBackend::Wyoming, "hello", Some("en-US")).unwrap(),
            r#"{"type":"synthesize","data":{"text":"hello","voice":{"name":"en-US"}}}"#
        );
    }

    #[test]
    fn barge_in_maps_to_control_message_and_cancel_event() {
        for raw in [
            r#"{"type":"barge_in"}"#,
            r#"{"type":"user-event","data":{"name":"barge_in","data":{}}}"#,
        ] {
            let action = parse_inbound(raw, false);
            assert_eq!(action, InboundAction::BargeIn);
            let message = channel(false).message_for_action(action).unwrap();
            assert!(message.content.is_empty());
        }

        assert_eq!(
            encode_tts_cancel(VoiceHostBackend::Native).unwrap(),
            r#"{"type":"tts_cancel"}"#
        );
        assert_eq!(
            encode_tts_cancel(VoiceHostBackend::Wyoming).unwrap(),
            r#"{"type":"user-event","data":{"name":"tts_cancel","data":{}}}"#
        );
    }

    #[test]
    fn transcript_replay_and_ack_encode_for_native_and_wyoming_backends() {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &encode_transcript_ack(VoiceHostBackend::Native, Some("final-1")).unwrap()
            )
            .unwrap(),
            serde_json::json!({
                "type": "transcript_ack",
                "event_id": "final-1",
            })
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &encode_transcript_replay_required(VoiceHostBackend::Wyoming, Some("final-2"),)
                    .unwrap()
            )
            .unwrap(),
            serde_json::json!({
                "type": "user-event",
                "data": {
                    "name": "transcript_replay_required",
                    "data": {
                        "event_id": "final-2",
                        "retryable": true,
                        "reconnect": true,
                    },
                },
            })
        );
    }

    #[test]
    fn approval_responses_map_all_supported_decisions() {
        for (decision, expected) in [
            ("approve", ChannelApprovalResponse::Approve),
            ("deny", ChannelApprovalResponse::Deny),
            ("always", ChannelApprovalResponse::AlwaysApprove),
        ] {
            let raw = format!(
                r#"{{"type":"user-event","data":{{"name":"approval_response","data":{{"request_id":"r1","decision":"{decision}"}}}}}}"#
            );
            assert_eq!(
                parse_inbound(&raw, false),
                InboundAction::Approval {
                    request_id: "r1".into(),
                    decision: expected,
                }
            );
        }
    }

    #[test]
    fn malformed_unknown_and_server_direction_events_do_no_model_work() {
        for raw in [
            "not-json",
            r#"{"type":"unknown"}"#,
            r#"{"type":"say","text":"wrong direction"}"#,
            r#"{"type":"tts_cancel"}"#,
            r#"{"type":"tts_chunk","audio_b64":"AAAA"}"#,
            r#"{"type":"user-event","data":{"name":"approval_response","data":{"request_id":"r1","decision":"later"}}}"#,
        ] {
            assert_eq!(parse_inbound(raw, true), InboundAction::None, "{raw}");
        }
    }

    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn websocket_flow_authenticates_and_round_trips_voice_controls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let observed_auth = Arc::new(std::sync::Mutex::new(None::<String>));
        let server_auth = observed_auth.clone();

        let server = zeroclaw_spawn::spawn!(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    *server_auth.lock().unwrap() = request
                        .headers()
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    Ok(response)
                },
            )
            .await
            .unwrap();

            socket
                .send(Message::Text(
                    r#"{"type":"speech_end","transcript":"book a meeting"}"#.into(),
                ))
                .await
                .unwrap();
            let ack = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let reply = socket.next().await.unwrap().unwrap().into_text().unwrap();

            socket
                .send(Message::Text(r#"{"type":"barge_in"}"#.into()))
                .await
                .unwrap();
            let cancel = socket.next().await.unwrap().unwrap().into_text().unwrap();

            let approval = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let approval_json: Value = serde_json::from_str(approval.as_str()).unwrap();
            let request_id = approval_json
                .pointer("/data/data/request_id")
                .and_then(Value::as_str)
                .unwrap();
            socket
                .send(Message::Text(
                    format!(
                        r#"{{"type":"user-event","data":{{"name":"approval_response","data":{{"request_id":"{request_id}","decision":"approve"}}}}}}"#
                    )
                    .into(),
                ))
                .await
                .unwrap();

            (
                ack.to_string(),
                reply.to_string(),
                cancel.to_string(),
                approval.to_string(),
            )
        });

        let channel = Arc::new(
            VoiceHostChannel::new(
                "office".into(),
                VoiceHostConfig {
                    enabled: true,
                    // Assemble the loopback test URL without an insecure production literal.
                    url: format!("ws:{0}{0}{address}", '/'),
                    api_key: Some("test-token".into()),
                    approval_timeout_secs: 5,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let listener_channel = channel.clone();
        let channel_listener = zeroclaw_spawn::spawn!(async move {
            listener_channel.listen(inbound_tx).await.unwrap();
        });

        let transcript = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(transcript.content, "book a meeting");

        channel
            .send(&SendMessage::new("meeting booked", "office"))
            .await
            .unwrap();
        let interrupt = tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(interrupt.content.is_empty());

        let approval = channel
            .request_approval_attributed(
                "office",
                &ChannelApprovalRequest {
                    tool_name: "calendar_create".into(),
                    arguments_summary: "Tomorrow at 09:00".into(),
                    raw_arguments: Some(serde_json::json!({"secret": "never-send"})),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(approval.response, ChannelApprovalResponse::Approve);
        assert_eq!(approval.source, ApprovalSource::Operator);

        let (ack, reply, cancel, approval_payload) =
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&ack).unwrap()["type"],
            "transcript_ack"
        );
        assert_eq!(reply, r#"{"type":"say","text":"meeting booked"}"#);
        assert_eq!(cancel, r#"{"type":"tts_cancel"}"#);
        assert!(!approval_payload.contains("never-send"));
        assert_eq!(
            observed_auth.lock().unwrap().as_deref(),
            Some("Bearer test-token")
        );

        channel_listener.abort();
        let _ = channel_listener.await;
    }

    #[tokio::test]
    async fn disconnected_approval_fails_closed_as_unreachable() {
        let response = channel(false)
            .request_approval_attributed(
                "office",
                &ChannelApprovalRequest {
                    tool_name: "shell".into(),
                    arguments_summary: "Run command".into(),
                    raw_arguments: None,
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(response.response, ChannelApprovalResponse::Deny);
        assert_eq!(response.source, ApprovalSource::Unreachable);
    }

    #[tokio::test]
    async fn approval_timeout_includes_waiting_for_outbound_queue_capacity() {
        let mut channel = channel(false);
        channel.approval_timeout_secs = 1;
        let (outbound_tx, _outbound_rx) = mpsc::channel(1);
        outbound_tx.send("queue-is-full".into()).await.unwrap();
        *channel.outbound.write().await = Some(outbound_tx);

        let response = tokio::time::timeout(
            Duration::from_millis(1_500),
            channel.request_approval_attributed(
                "office",
                &ChannelApprovalRequest {
                    tool_name: "shell".into(),
                    arguments_summary: "Run command".into(),
                    raw_arguments: None,
                },
            ),
        )
        .await
        .expect("approval operation must honor its configured timeout")
        .unwrap()
        .unwrap();

        assert_eq!(response.response, ChannelApprovalResponse::Deny);
        assert_eq!(response.source, ApprovalSource::TimedOut);
        assert!(channel.pending_approvals.lock().await.is_empty());
    }

    #[tokio::test]
    async fn aborting_approval_cleans_pending_request() {
        let channel = Arc::new(channel(false));
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        *channel.outbound.write().await = Some(outbound_tx);

        let request_channel = Arc::clone(&channel);
        let request = zeroclaw_spawn::spawn!(async move {
            request_channel
                .request_approval_attributed(
                    "office",
                    &ChannelApprovalRequest {
                        tool_name: "shell".into(),
                        arguments_summary: "Run command".into(),
                        raw_arguments: None,
                    },
                )
                .await
        });

        outbound_rx
            .recv()
            .await
            .expect("approval request should be queued before cancellation");
        assert_eq!(channel.pending_approvals.lock().await.len(), 1);

        request.abort();
        let _ = request.await;
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if channel.pending_approvals.lock().await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted approval should remove its pending responder");
    }

    #[test]
    fn malformed_api_key_is_rejected_without_echoing_secret() {
        let error = build_auth_headers(Some("top-secret\nsecond-line")).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("Authorization"));
        assert!(!text.contains("top-secret"));
    }
}
