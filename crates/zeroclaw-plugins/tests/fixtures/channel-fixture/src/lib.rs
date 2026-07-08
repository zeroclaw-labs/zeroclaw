//! Minimal ZeroClaw WIT channel plugin used as an integration-test fixture.
//!
//! It reports one canned inbound message the first time `poll-message` is
//! called, accepts `send` (dropping the bytes), advertises `health-check` +
//! `self-handle`, and stubs every capability-gated method with its documented
//! default. It exists solely to prove the host's channel-plugin runtime path
//! end-to-end (`WasmChannel::from_wasm` → configure → capabilities → send →
//! poll). No network, no filesystem, no config needed.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::cell::Cell;

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;

    const PLUGIN_NAME: &str = "echo-channel";
    const PLUGIN_VERSION: &str = "0.1.0";

    struct EchoChannel;

    // Deliver the canned inbound message exactly once, so a host poll loop sees
    // one message and then `none`.
    thread_local! {
        static DELIVERED: Cell<bool> = const { Cell::new(false) };
    }

    impl PluginInfo for EchoChannel {
        fn plugin_name() -> String {
            PLUGIN_NAME.to_string()
        }
        fn plugin_version() -> String {
            PLUGIN_VERSION.to_string()
        }
    }

    impl Channel for EchoChannel {
        fn name() -> String {
            PLUGIN_NAME.to_string()
        }

        fn configure(_config: String) -> Result<(), String> {
            Ok(())
        }

        fn send(_message: SendMessage) -> Result<(), String> {
            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            let already = DELIVERED.with(|d| d.replace(true));
            if already {
                return None;
            }
            Some(InboundMessage {
                id: "fixture-1".to_string(),
                sender: "tester".to_string(),
                reply_target: "tester".to_string(),
                content: "ping".to_string(),
                channel: PLUGIN_NAME.to_string(),
                channel_alias: None,
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: Vec::new(),
                subject: None,
            })
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            ChannelCapabilities::HEALTH_CHECK | ChannelCapabilities::SELF_HANDLE
        }

        fn health_check() -> bool {
            true
        }

        fn self_handle() -> Option<String> {
            Some("@echo".to_string())
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

        fn update_draft(_r: String, _m: String, _t: String) -> Result<(), String> {
            Ok(())
        }

        fn update_draft_progress(_r: String, _m: String, _t: String) -> Result<(), String> {
            Ok(())
        }

        fn finalize_draft(_r: String, _m: String, _t: String) -> Result<(), String> {
            Ok(())
        }

        fn cancel_draft(_r: String, _m: String) -> Result<(), String> {
            Ok(())
        }

        fn supports_multi_message_streaming() -> bool {
            false
        }

        fn multi_message_delay_ms() -> u64 {
            800
        }

        fn add_reaction(_c: String, _m: String, _e: String) -> Result<(), String> {
            Ok(())
        }

        fn remove_reaction(_c: String, _m: String, _e: String) -> Result<(), String> {
            Ok(())
        }

        fn pin_message(_c: String, _m: String) -> Result<(), String> {
            Ok(())
        }

        fn unpin_message(_c: String, _m: String) -> Result<(), String> {
            Ok(())
        }

        fn redact_message(_c: String, _m: String, _reason: Option<String>) -> Result<(), String> {
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

    export!(EchoChannel);
}
