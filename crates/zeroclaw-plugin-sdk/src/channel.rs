//! Ergonomics for the `channel-plugin` world. Enable the `channel`
//! feature and implement [`ChannelPlugin`], then call
//! [`crate::export_channel!`] once with your type.
//!
//! `websocket`/`gateway`/`http-helpers` host imports are re-exported as-is
//! under [`websocket`], [`gateway`], and [`http_helpers`] — they're already
//! idiomatic generated bindings (resource types with methods, or plain
//! functions) that don't need a wrapper.

use crate::bindings::channel::zeroclaw::plugin::{logging, plugin_config};

pub use crate::bindings::channel::exports::zeroclaw::plugin::channel::{
    ApprovalRequest, ApprovalResponse, ChannelCapabilities, InboundMessage, MediaAttachment,
    SendMessage,
};
pub use crate::bindings::channel::zeroclaw::plugin::gateway;
pub use crate::bindings::channel::zeroclaw::plugin::http_helpers;
pub use crate::bindings::channel::zeroclaw::plugin::websocket;
pub use logging::{LogLevel, PluginAction, PluginEvent, PluginOutcome};

/// Read a secret this plugin declared in its manifest's `declared_secrets`
/// list. Returns `None` for any key not declared, or not provided by the
/// operator for this instance.
pub fn get_secret(key: &str) -> Option<String> {
    plugin_config::get_secret(key)
}

/// The proxy URL applied to this plugin instance's outbound networking, if
/// any.
pub fn get_proxy_url() -> Option<String> {
    plugin_config::get_proxy_url()
}

/// Builder for a `logging::log-record` event. See `tool::LogEvent` for the
/// rationale — each world gets its own copy since `wit_bindgen::generate!`
/// produces a nominally distinct (structurally identical) type per world.
pub struct LogEvent {
    inner: PluginEvent,
}

impl LogEvent {
    pub fn new(
        function_name: impl Into<String>,
        action: PluginAction,
        message: impl Into<String>,
    ) -> Self {
        Self {
            inner: PluginEvent {
                function_name: function_name.into(),
                action,
                outcome: None,
                duration_ms: None,
                attrs: None,
                message: message.into(),
            },
        }
    }

    pub fn outcome(mut self, outcome: PluginOutcome) -> Self {
        self.inner.outcome = Some(outcome);
        self
    }

    pub fn success(self) -> Self {
        self.outcome(PluginOutcome::Success)
    }

    pub fn failure(self) -> Self {
        self.outcome(PluginOutcome::Failure)
    }

    pub fn duration_ms(mut self, ms: u64) -> Self {
        self.inner.duration_ms = Some(ms);
        self
    }

    pub fn attrs_json(mut self, json: impl Into<String>) -> Self {
        self.inner.attrs = Some(json.into());
        self
    }
}

pub fn log(level: LogLevel, event: LogEvent) {
    logging::log_record(level, &event.inner);
}

pub fn trace(function_name: &str, action: PluginAction, message: &str) {
    log(
        LogLevel::Trace,
        LogEvent::new(function_name, action, message),
    );
}

pub fn debug(function_name: &str, action: PluginAction, message: &str) {
    log(
        LogLevel::Debug,
        LogEvent::new(function_name, action, message),
    );
}

pub fn info(function_name: &str, action: PluginAction, message: &str) {
    log(
        LogLevel::Info,
        LogEvent::new(function_name, action, message),
    );
}

pub fn warn(function_name: &str, action: PluginAction, message: &str) {
    log(
        LogLevel::Warn,
        LogEvent::new(function_name, action, message),
    );
}

pub fn error(function_name: &str, action: PluginAction, message: &str) {
    log(
        LogLevel::Error,
        LogEvent::new(function_name, action, message),
    );
}

/// The trait a plugin author implements for a `channel-plugin` world.
///
/// Required methods (`name`, `send`, `poll_message`,
/// `get_channel_capabilities`) must be implemented. Capability-gated
/// methods have default bodies matching the stub values
/// `wit/v0/channel.wit` documents for each one verbatim (e.g.
/// `health_check` -> `true`, `multi_message_delay_ms` -> `800`,
/// `self_handle` -> `None`) — without these defaults, a plugin
/// implementing only the required subset would have to hand-write ~20
/// stub functions. [`ChannelPlugin::get_channel_capabilities`] tells the
/// host which of these are real vs. defaulted; the host only calls a
/// capability-gated method when its flag is set.
pub trait ChannelPlugin {
    fn plugin_info() -> (&'static str, &'static str);
    fn name() -> String;
    fn get_channel_capabilities() -> ChannelCapabilities;
    fn send(message: SendMessage) -> Result<(), String>;
    fn poll_message() -> Option<InboundMessage>;

    // ── Capability-gated, defaulted per wit/v0/channel.wit's documented stub values ──

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

    fn send_draft(_message: SendMessage) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn update_draft(_recipient: String, _message_id: String, _text: String) -> Result<(), String> {
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
        _text: String,
    ) -> Result<(), String> {
        Ok(())
    }

    fn cancel_draft(_recipient: String, _message_id: String) -> Result<(), String> {
        Ok(())
    }

    fn multi_message_delay_ms() -> u64 {
        800
    }

    fn add_reaction(
        _channel_id: String,
        _message_id: String,
        _emoji: String,
    ) -> Result<(), String> {
        Ok(())
    }

    fn remove_reaction(
        _channel_id: String,
        _message_id: String,
        _emoji: String,
    ) -> Result<(), String> {
        Ok(())
    }

    fn pin_message(_channel_id: String, _message_id: String) -> Result<(), String> {
        Ok(())
    }

    fn unpin_message(_channel_id: String, _message_id: String) -> Result<(), String> {
        Ok(())
    }

    fn redact_message(
        _channel_id: String,
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
}
