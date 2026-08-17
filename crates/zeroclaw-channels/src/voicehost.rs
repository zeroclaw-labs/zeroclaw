//! External voice host channel.

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue, header};
use uuid::Uuid;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    ApprovalSource, AttributedApprovalResponse, Channel, ChannelApprovalRequest,
    ChannelApprovalResponse, ChannelMessage, SendMessage, VoiceEvent,
};
use zeroclaw_config::schema::{VoiceHostConfig, ws_connect_with_proxy_headers};

const OUTBOUND_CAPACITY: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_TRANSCRIPT_BYTES: usize = 16 * 1024;
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
    FinalTranscript(String),
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
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
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
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn message_for_action(&self, action: InboundAction) -> Option<ChannelMessage> {
        let (content, passive_context, interrupt_only) = match action {
            InboundAction::FinalTranscript(text) => (text, false, false),
            InboundAction::PartialTranscript(text) => (text, true, false),
            InboundAction::BargeIn => (String::new(), false, true),
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
            interrupt_only,
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

    async fn handle_barge_in<S>(
        &self,
        tx: &mpsc::Sender<ChannelMessage>,
        write: &mut S,
    ) -> Result<BargeInOutcome>
    where
        S: futures_util::Sink<Message> + Unpin,
    {
        if let Some(message) = self.message_for_action(InboundAction::BargeIn)
            && tx.send(message).await.is_err()
        {
            return Ok(BargeInOutcome::DispatchClosed);
        }

        let cancel = encode_tts_cancel(self.backend)?;
        if write.send(Message::Text(cancel.into())).await.is_err() {
            return Ok(BargeInOutcome::RemoteClosed);
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
        self.queue_payload(encode_reply(
            self.backend,
            &message.content,
            self.voice.as_deref(),
        )?)
        .await
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut reconnect_attempt = 0usize;

        loop {
            if tx.is_closed() {
                self.clear_connection_state().await;
                return Ok(());
            }

            let service_key = format!("channel.voicehost.{}", self.alias);
            let connected = tokio::select! {
                result = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    ws_connect_with_proxy_headers(
                        &self.url,
                        &service_key,
                        self.proxy_url.as_deref(),
                        &self.headers,
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
            *self.outbound.write().await = Some(outbound_tx);
            let mut ping = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
            ping.tick().await;
            let mut dispatch_closed = false;
            let mut last_partial_forwarded = None;

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
                    _ = tx.closed() => {
                        dispatch_closed = true;
                        break;
                    }
                    _ = ping.tick() => {
                        if write.send(Message::Ping(Vec::new().into())).await.is_err() {
                            break;
                        }
                    }
                    payload = outbound_rx.recv() => {
                        let Some(payload) = payload else { break };
                        if write.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    incoming = read.next() => {
                        let raw = match incoming {
                            Some(Ok(Message::Text(text))) => text,
                            Some(Ok(Message::Ping(payload))) => {
                                if write.send(Message::Pong(payload)).await.is_err() {
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
                                match self.handle_barge_in(&tx, &mut write).await? {
                                    BargeInOutcome::Continue => {}
                                    BargeInOutcome::DispatchClosed => {
                                        dispatch_closed = true;
                                        break;
                                    }
                                    BargeInOutcome::RemoteClosed => break,
                                }
                            }
                            action @ InboundAction::FinalTranscript(_) => {
                                if let Some(message) = self.message_for_action(action)
                                    && tx.send(message).await.is_err()
                                {
                                    dispatch_closed = true;
                                    break;
                                }
                            }
                            action @ InboundAction::PartialTranscript(_) => {
                                let now = Instant::now();
                                if last_partial_forwarded.is_some_and(|last: Instant| {
                                    now.duration_since(last) < PARTIAL_FORWARD_INTERVAL
                                }) {
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
                .map(InboundAction::FinalTranscript)
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
            .map(InboundAction::FinalTranscript)
            .unwrap_or(InboundAction::None),
        "transcript-chunk" if forward_partials => wyoming_text(&value)
            .map(InboundAction::PartialTranscript)
            .unwrap_or(InboundAction::None),
        "user-event" => parse_wyoming_user_event(&value),
        _ => InboundAction::None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};
    use zeroclaw_api::channel::ChannelApprovalResponse;
    use zeroclaw_config::schema::VoiceHostConfig;

    struct FailingSink;

    impl futures_util::Sink<Message> for FailingSink {
        type Error = anyhow::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            anyhow::bail!("remote closed")
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

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
    async fn barge_in_reaches_local_dispatch_when_remote_cancel_write_fails() {
        let channel = channel(false);
        let (tx, mut rx) = mpsc::channel(1);
        let mut failing_remote = FailingSink;

        assert_eq!(
            channel
                .handle_barge_in(&tx, &mut failing_remote)
                .await
                .unwrap(),
            BargeInOutcome::RemoteClosed
        );
        assert!(rx.recv().await.unwrap().interrupt_only);
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
        assert_eq!(action, InboundAction::FinalTranscript("hello world".into()));

        let message = channel(false).message_for_action(action).unwrap();
        assert_eq!(message.content, "hello world");
        assert_eq!(message.channel, "voicehost");
        assert_eq!(message.channel_alias.as_deref(), Some("office"));
        assert_eq!(message.reply_target, "office");
        assert!(message.explicitly_addressed);
        assert!(!message.passive_context);
        assert!(!message.interrupt_only);
    }

    #[test]
    fn wyoming_final_transcript_maps_to_final_action() {
        assert_eq!(
            parse_inbound(
                r#"{"type":"transcript","data":{"text":"hello from wyoming"}}"#,
                false,
            ),
            InboundAction::FinalTranscript("hello from wyoming".into())
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
        assert!(!message.interrupt_only);
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
        assert!(interrupt.interrupt_only);
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
    fn barge_in_maps_to_interrupt_only_and_cancel_control() {
        for raw in [
            r#"{"type":"barge_in"}"#,
            r#"{"type":"user-event","data":{"name":"barge_in","data":{}}}"#,
        ] {
            let action = parse_inbound(raw, false);
            assert_eq!(action, InboundAction::BargeIn);
            let message = channel(false).message_for_action(action).unwrap();
            assert!(message.interrupt_only);
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

            (reply.to_string(), cancel.to_string(), approval.to_string())
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
        assert!(interrupt.interrupt_only);

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

        let (reply, cancel, approval_payload) =
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
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

    #[test]
    fn malformed_api_key_is_rejected_without_echoing_secret() {
        let error = build_auth_headers(Some("top-secret\nsecond-line")).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("Authorization"));
        assert!(!text.contains("top-secret"));
    }
}
