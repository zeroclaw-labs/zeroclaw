//! Minimal channel component used by the plugin-host integration tests.

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

    struct FixtureChannel;
    static HTTP_CALL_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    /// Optional handle taken from the host-provided `configure` payload, so
    /// host tests can observe which config generation an instance was
    /// configured with (reconstruction must replay the constructor snapshot).
    static CONFIGURED_HANDLE: Mutex<Option<String>> = Mutex::new(None);

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

        fn configure(config: String) -> Result<(), String> {
            let parsed: serde_json::Value = serde_json::from_str(&config).unwrap_or_default();
            // Typed-config contract: the host must deliver `retry_count` as a
            // JSON integer, not the operator's raw string. Checked on the
            // parsed value rather than the serialized text so an additional
            // property (and any key ordering) does not break the assertion.
            if parsed["retry_count"].as_u64() != Some(5) {
                return Err("expected typed retry_count config".to_string());
            }
            if let Some(handle) = parsed["handle"].as_str() {
                *CONFIGURED_HANDLE.lock().unwrap() = Some(handle.to_string());
            }
            Ok(())
        }

        fn send(message: SendMessage) -> Result<(), String> {
            if message.content.starts_with("http://") {
                if HTTP_CALL_IN_FLIGHT.swap(true, Ordering::SeqCst) {
                    return Err("interrupted channel instance was resumed".to_string());
                }
                waki::Client::new()
                    .get(&message.content)
                    .send()
                    .and_then(waki::Response::body)
                    .map_err(|error| error.to_string())?;
                HTTP_CALL_IN_FLIGHT.store(false, Ordering::SeqCst);
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
            ChannelCapabilities::HEALTH_CHECK | ChannelCapabilities::SELF_HANDLE
        }

        fn health_check() -> bool {
            true
        }

        fn self_handle() -> Option<String> {
            Some(
                CONFIGURED_HANDLE
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "@fixture".to_string()),
            )
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
