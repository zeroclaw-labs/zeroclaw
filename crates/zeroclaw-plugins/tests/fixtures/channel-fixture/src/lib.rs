//! Minimal channel component used by the plugin-host scoped-secret tests.

#[cfg(all(target_family = "wasm", not(feature = "socket-e2e")))]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use zeroclaw::plugin::config::{ConfigError, get as config_get};
    use zeroclaw::plugin::secrets::{SecretError, get as secret_get};
    use zeroclaw::plugin::state::{StateError, get as state_get, put as state_put};

    struct FixtureChannel;

    fn current_public_config() -> Result<serde_json::Value, String> {
        let config = config_get().map_err(|_| "expected point-of-use public config".to_string())?;
        serde_json::from_str(&config).map_err(|_| "expected public config object".to_string())
    }

    impl PluginInfo for FixtureChannel {
        fn plugin_name() -> String {
            "channel-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Channel for FixtureChannel {
        fn name() -> String {
            "channel-fixture".to_string()
        }

        fn configure() -> Result<(), String> {
            let config = current_public_config()?;
            let public = config
                .as_object()
                .ok_or_else(|| "expected public config object".to_string())?;
            if public
                .get("retry_count")
                .and_then(serde_json::Value::as_u64)
                != Some(5)
            {
                return Err("expected typed retry_count config".to_string());
            }
            if public
                .get("credential_epoch")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err("expected credential_epoch config".to_string());
            }
            if public.len() != 2 {
                return Err("expected only public config".to_string());
            }
            if !matches!(secret_get("retry_count"), Err(SecretError::NotFound)) {
                return Err("public property was exposed as a secret".to_string());
            }
            let token = secret_get("api_token")
                .map_err(|_| "expected scoped api_token secret".to_string())?;
            if token.is_empty() {
                return Err("expected non-empty api_token secret".to_string());
            }
            let current = state_get("channel-session")
                .map_err(|_| "expected scoped channel state".to_string())?;
            let expected = current.as_ref().map(|entry| entry.revision);
            let revision = state_put("channel-session", token.as_bytes(), expected)
                .map_err(|_| "expected scoped channel state write".to_string())?;
            if revision != expected.unwrap_or(0) + 1 {
                return Err("unexpected channel state revision".to_string());
            }

            Ok(())
        }

        fn send(message: SendMessage) -> Result<(), String> {
            let config = current_public_config()?;
            let epoch = config
                .get("credential_epoch")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "expected credential_epoch config".to_string())?;
            if !matches!(secret_get("retry_count"), Err(SecretError::NotFound)) {
                return Err("public property was exposed as a secret".to_string());
            }
            let token = secret_get("api_token")
                .map_err(|_| "expected api_token during channel operation".to_string())?;
            let state = state_get("channel-session")
                .map_err(|_| "expected channel state during operation".to_string())?
                .ok_or_else(|| "expected configured channel state".to_string())?;
            if state.value != token.as_bytes() {
                let next_revision =
                    state_put("channel-session", token.as_bytes(), Some(state.revision))
                        .map_err(|_| "expected CAS update after credential rotation".to_string())?;
                if next_revision != state.revision + 1 {
                    return Err("unexpected rotated channel state revision".to_string());
                }
            }
            if message.content != format!("{epoch}:{token}") {
                return Err("message did not use one current config revision".to_string());
            }

            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            let message = zeroclaw::plugin::inbound::inbound_poll()?;
            Some(InboundMessage {
                id: message.id,
                sender: message.sender,
                reply_target: message.reply_target,
                content: message.content,
                // Deliberately untrusted: the host must replace both values
                // with its admitted logical endpoint.
                channel: "guest-channel".to_string(),
                channel_alias: Some("guest-alias".to_string()),
                timestamp: message.timestamp,
                thread_ts: message.thread_ts,
                interruption_scope_id: message.interruption_scope_id,
                attachments: Vec::new(),
                subject: message.subject,
            })
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            if matches!(config_get(), Err(ConfigError::Unavailable))
                && matches!(secret_get("api_token"), Err(SecretError::Unavailable))
                && matches!(state_get("channel-session"), Err(StateError::Unavailable))
            {
                ChannelCapabilities::HEALTH_CHECK | ChannelCapabilities::SELF_HANDLE
            } else {
                ChannelCapabilities::empty()
            }
        }

        fn health_check() -> bool {
            true
        }

        fn self_handle() -> Option<String> {
            (matches!(config_get(), Err(ConfigError::Unavailable))
                && matches!(secret_get("api_token"), Err(SecretError::Unavailable))
                && matches!(state_get("channel-session"), Err(StateError::Unavailable)))
            .then(|| "@fixture".to_string())
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
            800
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
    }

    export!(FixtureChannel);
}

#[cfg(all(target_family = "wasm", feature = "socket-e2e"))]
mod socket_component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0", "plugins-wit-v0-sockets"],
    });

    use std::cell::{Cell, RefCell};

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use zeroclaw::plugin::config::get as config_get;
    use zeroclaw::plugin::sockets::{
        ConnectMode, ConnectRequest, Connection, ReceiveEvent, connect,
    };

    struct SocketFixtureChannel;

    thread_local! {
        static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
        static SENT: Cell<bool> = const { Cell::new(false) };
        static COMPLETE: Cell<bool> = const { Cell::new(false) };
    }

    fn inbound(content: String) -> InboundMessage {
        InboundMessage {
            id: "socket-fixture-1".to_string(),
            sender: "socket-peer".to_string(),
            reply_target: "socket-peer".to_string(),
            content,
            channel: "untrusted-guest-channel".to_string(),
            channel_alias: Some("untrusted-guest-alias".to_string()),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
            subject: None,
        }
    }

    impl PluginInfo for SocketFixtureChannel {
        fn plugin_name() -> String {
            "socket-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Channel for SocketFixtureChannel {
        fn name() -> String {
            "socket-fixture".to_string()
        }

        fn configure() -> Result<(), String> {
            let config = config_get().map_err(|_| "config-unavailable".to_string())?;
            let config: serde_json::Value =
                serde_json::from_str(&config).map_err(|_| "config-invalid".to_string())?;
            let host = config
                .get("host")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "host-missing".to_string())?;
            let port = config
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| "port-invalid".to_string())?;
            let connection = connect(&ConnectRequest {
                host: host.to_string(),
                port,
                mode: ConnectMode::Plaintext,
                tls_profile: None,
            })
            .map_err(|error| format!("{error:?}"))?;
            CONNECTION.with(|slot| slot.replace(Some(connection)));
            Ok(())
        }

        fn send(_message: SendMessage) -> Result<(), String> {
            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            if COMPLETE.with(Cell::get) {
                return None;
            }
            CONNECTION.with(|slot| {
                let slot = slot.borrow();
                let connection = slot.as_ref()?;
                if !SENT.with(|sent| sent.replace(true))
                    && let Err(error) = connection.send(b"component-ping")
                {
                    COMPLETE.with(|complete| complete.set(true));
                    return Some(inbound(format!("{error:?}")));
                }
                match connection.receive() {
                    Ok(ReceiveEvent::Data(bytes)) => {
                        COMPLETE.with(|complete| complete.set(true));
                        Some(inbound(String::from_utf8_lossy(&bytes).into_owned()))
                    }
                    Ok(ReceiveEvent::Idle) => None,
                    Ok(ReceiveEvent::Closed(reason)) => {
                        COMPLETE.with(|complete| complete.set(true));
                        Some(inbound(format!("closed:{reason:?}")))
                    }
                    Err(error) => {
                        COMPLETE.with(|complete| complete.set(true));
                        Some(inbound(format!("{error:?}")))
                    }
                }
            })
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            ChannelCapabilities::HEALTH_CHECK
        }

        fn health_check() -> bool {
            true
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
            800
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
    }

    export!(SocketFixtureChannel);
}
