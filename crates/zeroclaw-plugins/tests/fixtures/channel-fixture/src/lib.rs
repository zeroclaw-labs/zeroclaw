//! Minimal ZeroClaw WIT channel plugin used as an integration-test fixture.
//!
//! It reports one canned inbound message the first time `poll-message` is
//! called, accepts `send` (dropping the bytes), advertises `health-check` +
//! `self-handle`, and stubs every capability-gated method with its documented
//! default. Its `configure` export emits a unique host log so integration tests
//! can prove whether startup exports ran. It exists solely to prove the host's
//! channel-plugin runtime path end-to-end (`WasmChannel::from_wasm` → configure
//! → capabilities → send → poll). No network, no filesystem, no config
//! needed.
//!
//! The host E2E tests build this source on demand with the checked-in lockfile.
//! Manual build: `cargo build --locked --target wasm32-wasip2`.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0"],
    });

    use std::cell::{Cell, RefCell};

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage, WebhookRejection,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use zeroclaw::plugin::logging::{
        log_record, LogLevel, PluginAction, PluginEvent, PluginOutcome,
    };

    const PLUGIN_NAME: &str = "echo-channel";
    const PLUGIN_VERSION: &str = "0.1.0";
    const CONFIGURE_MARKER: &str = "channel-fixture configure export invoked";
    const POLL_MARKER: &str = "channel-fixture poll-message export invoked";

    struct EchoChannel;

    // Deliver one inbound message, then `none`, so a host poll loop terminates.
    // The message echoes the JSON this plugin received from `configure`, so a
    // host test can assert exactly what config (plaintext, typed) reached it.
    thread_local! {
        static DELIVERED: Cell<bool> = const { Cell::new(false) };
        static CONFIG: RefCell<String> = RefCell::new(String::new());
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

        fn configure(config: String) -> Result<(), String> {
            log_record(
                LogLevel::Info,
                &PluginEvent {
                    function_name: "channel_fixture::configure".to_string(),
                    action: PluginAction::Start,
                    outcome: Some(PluginOutcome::Success),
                    duration_ms: None,
                    attrs: None,
                    message: CONFIGURE_MARKER.to_string(),
                },
            );
            CONFIG.with(|c| *c.borrow_mut() = config);
            Ok(())
        }

        fn send(_message: SendMessage) -> Result<(), String> {
            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            log_record(
                LogLevel::Info,
                &PluginEvent {
                    function_name: "channel_fixture::poll_message".to_string(),
                    action: PluginAction::Inbound,
                    outcome: Some(PluginOutcome::Success),
                    duration_ms: None,
                    attrs: None,
                    message: POLL_MARKER.to_string(),
                },
            );
            let already = DELIVERED.with(|d| d.replace(true));
            if already {
                return None;
            }
            // Echo the config this plugin received, so the host test can assert
            // the exact (plaintext, typed) JSON that reached `configure`.
            let content = CONFIG.with(|c| c.borrow().clone());
            Some(InboundMessage {
                id: "fixture-1".to_string(),
                sender: "tester".to_string(),
                reply_target: "tester".to_string(),
                content,
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
            ChannelCapabilities::HEALTH_CHECK
                | ChannelCapabilities::SELF_HANDLE
                | ChannelCapabilities::WEBHOOK_INGRESS
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

        fn webhook_path() -> Option<String> {
            Some("fixture".to_string())
        }

        fn parse_webhook(
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        ) -> Result<Vec<InboundMessage>, WebhookRejection> {
            // Auth: the caller must present this fixture's configured secret in
            // `x-fixture-secret`; otherwise reject (the host replies 401 and
            // enqueues nothing). Stands in for a real platform HMAC check.
            let secret = CONFIG.with(|c| c.borrow().clone());
            let presented = headers
                .iter()
                .find(|(k, _)| k == "x-fixture-secret")
                .map(|(_, v)| v.as_str());
            if presented != Some(secret.as_str()) {
                return Err(WebhookRejection::Unauthorized(
                    "bad signature".to_string(),
                ));
            }
            if body == b"stall-parse" {
                // WASI clocks suspend through an async host call. The host E2E
                // cancels this invocation at the request deadline and then
                // proves the same warm channel store can process another call.
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
            let content = String::from_utf8(body).map_err(|_| {
                WebhookRejection::BadRequest("non-utf8 body".to_string())
            })?;
            Ok(vec![InboundMessage {
                id: "webhook-1".to_string(),
                sender: "webhook".to_string(),
                reply_target: "webhook".to_string(),
                content,
                channel: PLUGIN_NAME.to_string(),
                channel_alias: None,
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: Vec::new(),
                subject: None,
            }])
        }
    }

    export!(EchoChannel);
}
