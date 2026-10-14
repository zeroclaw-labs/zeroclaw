//! Minimal channel component that holds one host-mediated WebSocket across
//! polls, used by the root channel WebSocket lifecycle integration test.
//!
//! It is shaped like a gateway-style channel (a Discord Gateway or Slack Socket
//! Mode client) with the vendor protocol stripped away:
//!
//! - `configure` validates the `url` it will dial and dials nothing.
//! - `poll-message` dials lazily from the url resolved at point of use (at most
//!   one `connect` per poll), drains a bounded number of events, and turns each
//!   `msg:<sender>:<text>` text frame into one inbound message.
//! - `send` writes `out:<recipient>:<content>` on the same socket.
//! - A terminal event drops the connection resource, which closes the socket
//!   and releases the instance's egress lease, so the next poll redials.
//!
//! Message ids are `ws-<connections opened>-<frames mapped>`. Both counters are
//! guest globals, so they survive between exports only inside one warm store:
//! a rebuilt instance starts again at `ws-1-1`, and a redial on the same store
//! keeps counting. The test reads the lifecycle off those ids. The close that
//! ended the previous socket rides on the next inbound message's `subject`
//! (`close:<code>`), so the test can also read how the host reported it.
//!
//! This is a test double, not a template. It redials on every poll with no
//! backoff, and its dial is a networked wait inside `poll-message`. A real
//! channel plugin should back off between failed dials and keep each poll
//! short.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0", "plugins-wit-v0-websocket"],
    });

    use std::cell::RefCell;

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage, WebhookRejection, WebhookRequest, WebhookResponse,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    // Point-of-use config import: the channel world's `configure` takes no
    // argument, and the guest re-reads `url` at every dial rather than keeping
    // a snapshot in warm state.
    use zeroclaw::plugin::config::get as config_get;
    use zeroclaw::plugin::websocket::{
        CloseCode, ConnectOptions, Connection, Event, Message, WebsocketError, connect,
    };

    /// Most `receive` calls one poll makes.
    ///
    /// `receive` never blocks, and every call spends one of the call frame's
    /// host calls; once that budget is gone every import in the frame answers
    /// `unavailable`. A poll stops at the first empty receive anyway and the
    /// host queues at most a handful of events, so a normal poll spends a few
    /// calls. The cap bounds a peer that streams frames this fixture ignores,
    /// keeps the call far inside the budget and the export deadline, and hands
    /// pacing back to the host's poll loop instead of spinning here.
    const MAX_RECEIVES_PER_POLL: usize = 16;

    /// Guest-owned socket state. Config never lives here.
    #[derive(Default)]
    struct Session {
        /// The live socket; `Some` exactly while the guest believes it is up.
        connection: Option<Connection>,
        /// Sockets opened by this store, for the message id.
        connections: u32,
        /// Frames mapped to inbound messages by this store, for the message id.
        messages: u64,
        /// How the previous socket was closed, reported on the next message.
        last_close: Option<String>,
    }

    thread_local! {
        static SESSION: RefCell<Session> = RefCell::new(Session::default());
    }

    struct WebSocketChannel;

    impl PluginInfo for WebSocketChannel {
        fn plugin_name() -> String {
            "channel-websocket-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Channel for WebSocketChannel {
        fn name() -> String {
            "channel-websocket-fixture".to_string()
        }

        fn configure() -> Result<(), String> {
            // Validate only. Dialing here would tie reach to construction; the
            // test proves the host decides reach per connect instead.
            configured_url().map(|_| ())
        }

        fn send(message: SendMessage) -> Result<(), String> {
            SESSION.with_borrow_mut(|session| {
                if session.connection.is_none() {
                    open(session)?;
                }
                let frame = Message::Text(format!("out:{}:{}", message.recipient, message.content));
                let outcome = session
                    .connection
                    .as_ref()
                    .map(|connection| connection.send(&frame));
                match outcome {
                    Some(Ok(())) => Ok(()),
                    Some(Err(error)) => {
                        if matches!(error, WebsocketError::Closed) {
                            session.connection = None;
                        }
                        Err(format!("send:{error:?}"))
                    }
                    None => Err("send:not-connected".to_string()),
                }
            })
        }

        fn poll_message() -> Option<InboundMessage> {
            SESSION.with_borrow_mut(|session| {
                if session.connection.is_none() {
                    // One dial per poll. A refusal leaves the channel down and
                    // the next poll tries again, so a grant applied later is
                    // picked up without rebuilding the channel.
                    open(session).ok()?;
                }
                for _ in 0..MAX_RECEIVES_PER_POLL {
                    let received = session.connection.as_ref()?.receive();
                    match received {
                        Ok(None) => return None,
                        Ok(Some(Event::Message(Message::Text(frame)))) => {
                            if let Some(message) = inbound(session, &frame) {
                                return Some(message);
                            }
                        }
                        Ok(Some(Event::Message(Message::Binary(_)))) => {}
                        Ok(Some(Event::Closed(frame))) => {
                            session.last_close = Some(match frame {
                                Some(frame) => close_label(&frame.code),
                                None => "none".to_string(),
                            });
                            // Dropping the resource closes the socket and
                            // releases the host's connection lease.
                            session.connection = None;
                            return None;
                        }
                        Ok(Some(Event::Failed(_))) | Err(WebsocketError::Closed) => {
                            session.connection = None;
                            return None;
                        }
                        // Anything else (for example an exhausted host-call
                        // budget) says nothing about the socket: keep it.
                        Err(_) => return None,
                    }
                }
                None
            })
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            ChannelCapabilities::HEALTH_CHECK
        }

        fn health_check() -> bool {
            SESSION.with_borrow(|session| session.connection.is_some())
        }

        fn self_handle() -> Option<String> {
            None
        }

        fn self_addressed_mention() -> Option<String> {
            None
        }

        fn drop_self_message(_msg: InboundMessage) -> bool {
            false
        }

        fn start_typing(_recipient: String) -> Result<(), String> {
            Ok(())
        }

        fn stop_typing(_recipient: String) -> Result<(), String> {
            Ok(())
        }

        fn supports_draft_updates() -> bool {
            false
        }

        fn send_draft(_message: SendMessage) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn update_draft(
            _recipient: String,
            _message_id: String,
            _text: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update_draft_progress(
            _recipient: String,
            _message_id: String,
            _text: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn finalize_draft(
            _recipient: String,
            _message_id: String,
            _final_text: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn cancel_draft(_recipient: String, _message_id: String) -> Result<(), String> {
            Ok(())
        }

        fn supports_multi_message_streaming() -> bool {
            false
        }

        fn multi_message_delay_ms() -> u64 {
            0
        }

        fn add_reaction(
            _channel: String,
            _message_id: String,
            _emoji: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn remove_reaction(
            _channel: String,
            _message_id: String,
            _emoji: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn pin_message(_channel: String, _message_id: String) -> Result<(), String> {
            Ok(())
        }

        fn unpin_message(_channel: String, _message_id: String) -> Result<(), String> {
            Ok(())
        }

        fn redact_message(
            _channel: String,
            _message_id: String,
            _reason: Option<String>,
        ) -> Result<(), String> {
            Ok(())
        }

        fn request_approval(
            _recipient: String,
            _request: ApprovalRequest,
        ) -> Result<Option<ApprovalResponse>, String> {
            Ok(None)
        }

        fn request_choice(
            _question: String,
            _choices: Vec<String>,
            _timeout_secs: u64,
        ) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn supports_free_form_ask() -> bool {
            true
        }

        // The channel WIT contract requires these exports even when the fixture
        // does not advertise webhook ingress. Keep them inert so this component
        // stays focused on the socket lifecycle.
        fn webhook_path() -> Option<String> {
            None
        }

        fn parse_webhook(_request: WebhookRequest) -> Result<WebhookResponse, WebhookRejection> {
            Err(WebhookRejection::BadRequest("unsupported".to_string()))
        }
    }

    /// The `url` from point-of-use config, which must name a WebSocket scheme.
    fn configured_url() -> Result<String, String> {
        let raw = config_get().map_err(|error| format!("config:{error:?}"))?;
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|_| "config:not-json".to_string())?;
        let url = value
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "config:url-missing".to_string())?;
        if url.starts_with("ws://") || url.starts_with("wss://") {
            Ok(url.to_string())
        } else {
            Err("config:url-not-websocket".to_string())
        }
    }

    /// Dial the configured url once. Config is re-read at every dial.
    fn open(session: &mut Session) -> Result<(), String> {
        let connection = connect(&ConnectOptions {
            url: configured_url()?,
            headers: Vec::new(),
            subprotocols: Vec::new(),
            tls_profile: None,
        })
        .map_err(|error| format!("connect:{error:?}"))?;
        session.connections += 1;
        session.connection = Some(connection);
        Ok(())
    }

    /// Map one `msg:<sender>:<text>` frame to an inbound message.
    fn inbound(session: &mut Session, frame: &str) -> Option<InboundMessage> {
        let (sender, text) = frame.strip_prefix("msg:")?.split_once(':')?;
        if sender.is_empty() {
            return None;
        }
        // Counts every mapped frame, delivered or not: the host applies its
        // sender policy after this point, and the test reads the gap.
        session.messages += 1;
        Some(InboundMessage {
            id: format!("ws-{}-{}", session.connections, session.messages),
            sender: sender.to_string(),
            reply_target: sender.to_string(),
            content: text.to_string(),
            // Routing identity is host-issued; both hints are overridden.
            channel: "channel-websocket-fixture".to_string(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
            subject: session
                .last_close
                .take()
                .map(|close| format!("close:{close}")),
        })
    }

    /// The WIT close-code case the host reported, by its WIT name, so the test
    /// can tell a named case from `other(<code>)`.
    fn close_label(code: &CloseCode) -> String {
        let named = match code {
            CloseCode::Normal => "normal",
            CloseCode::GoingAway => "going-away",
            CloseCode::ProtocolError => "protocol-error",
            CloseCode::UnsupportedData => "unsupported-data",
            CloseCode::InvalidPayload => "invalid-payload",
            CloseCode::PolicyViolation => "policy-violation",
            CloseCode::MessageTooBig => "message-too-big",
            CloseCode::MandatoryExtension => "mandatory-extension",
            CloseCode::InternalError => "internal-error",
            CloseCode::ServiceRestart => "service-restart",
            CloseCode::TryAgainLater => "try-again-later",
            CloseCode::Other(code) => return format!("other:{code}"),
        };
        named.to_string()
    }

    export!(WebSocketChannel);
}
