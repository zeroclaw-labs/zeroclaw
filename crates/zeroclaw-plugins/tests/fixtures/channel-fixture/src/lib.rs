//! Minimal channel component used by the plugin-host scoped-secret tests.

#[cfg(target_family = "wasm")]
mod component {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

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

    struct FixtureChannel;
    static SEND_CALL_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    /// Optional handle taken from the host-provided `configure` payload, so
    /// host tests can observe which config generation an instance was
    /// configured with (reconstruction must replay the constructor snapshot).
    static CONFIGURED_HANDLE: Mutex<Option<String>> = Mutex::new(None);

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
            // Public config carries only non-secret properties. `api_token` is
            // secret and must never surface here. `handle` is an optional
            // non-secret the reconstruction test supplies; stash it so the
            // probed `self-handle` can surface it without a live config frame.
            if public.contains_key("api_token") {
                return Err("secret property leaked into public config".to_string());
            }
            if let Some(handle) = public.get("handle").and_then(serde_json::Value::as_str) {
                *CONFIGURED_HANDLE.lock().unwrap() = Some(handle.to_string());
            }
            if !matches!(secret_get("retry_count"), Err(SecretError::NotFound)) {
                return Err("public property was exposed as a secret".to_string());
            }
            let token = secret_get("api_token")
                .map_err(|_| "expected scoped api_token secret".to_string())?;
            if token.is_empty() {
                return Err("expected non-empty api_token secret".to_string());
            }

            Ok(())
        }

        fn send(message: SendMessage) -> Result<(), String> {
            // Deadline/interruption path: a `spin` message drives an unbounded
            // guest loop whose duration the host wall-clock deadline bounds.
            // Channels have no outbound-HTTP surface (the host's
            // `new_channel_store` withholds `wasi:http`), so the slow operation
            // under test is guest compute, not a network host import. The
            // in-flight guard proves an interrupted instance is discarded rather
            // than resumed: a rebuilt instance is a fresh store whose flag reads
            // false, while a wrongly resumed store would re-enter with it set.
            if message.content.starts_with("spin") {
                if SEND_CALL_IN_FLIGHT.swap(true, Ordering::SeqCst) {
                    return Err("interrupted channel instance was resumed".to_string());
                }
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }
            // Scoped-secret path (this PR): the message must have been composed
            // from one current config+secret revision resolved at point of use.
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
            if message.content != format!("{epoch}:{token}") {
                return Err("message did not use one current config revision".to_string());
            }

            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            let message = zeroclaw::plugin::inbound::inbound_poll()?;
            // Host tests use this to interrupt a poll after the message has
            // already been dequeued from the host-owned queue: the spin runs
            // until the host wall-clock deadline discards this instance.
            if message.content.starts_with("spin") {
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }
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
            // Static discovery runs outside a service frame, so config and
            // secrets are unavailable here; surfacing either would mean the host
            // ran this probe in the wrong phase. The handle stashed during
            // `configure` is replayed so the reconstruction metadata check sees
            // a stable value across a rebuilt instance.
            (matches!(config_get(), Err(ConfigError::Unavailable))
                && matches!(secret_get("api_token"), Err(SecretError::Unavailable)))
            .then(|| {
                CONFIGURED_HANDLE
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "@fixture".to_string())
            })
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
