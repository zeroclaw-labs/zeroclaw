//! Ergonomics for the `tool-plugin` world. Enable the `tool` feature and
//! implement [`ToolPlugin`], then call [`crate::export_tool!`] once with
//! your type.

use crate::bindings::tool::zeroclaw::plugin::{logging, plugin_config};

/// The WIT `tool-result` record, re-exported under a friendlier path.
pub type ToolResult = crate::bindings::tool::exports::zeroclaw::plugin::tool::ToolResult;
type RawToolResult = ToolResult;

pub use logging::{LogLevel, PluginAction, PluginEvent, PluginOutcome};

/// Read a secret this plugin declared in its manifest's `declared_secrets`
/// list. Returns `None` for any key not declared, or not provided by the
/// operator for this instance.
pub fn get_secret(key: &str) -> Option<String> {
    plugin_config::get_secret(key)
}

/// The proxy URL applied to this plugin instance's outbound networking, if
/// any. Informational — the host already applies the proxy to every
/// outbound call.
pub fn get_proxy_url() -> Option<String> {
    plugin_config::get_proxy_url()
}

/// Builder for a `logging::log-record` event. Removes the boilerplate of
/// constructing a [`PluginEvent`] by hand for the common case (function
/// name + action + message), while still allowing full control via builder
/// methods.
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

/// Emit at the given level. Fire-and-forget, matching `log-record`'s
/// signature exactly.
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

/// A tool's static metadata (name/description/JSON-Schema parameters).
/// Removes the boilerplate of three small methods that always travel
/// together; `execute` is left as a single plain function on
/// [`ToolPlugin`] since it's already a one-function interface not worth
/// abstracting further.
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters_schema: String,
}

impl ToolMetadata {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema: "{}".to_string(),
        }
    }

    /// Set the JSON-Schema for this tool's parameters, as a raw JSON
    /// string.
    pub fn parameters_schema(mut self, schema: impl Into<String>) -> Self {
        self.parameters_schema = schema.into();
        self
    }
}

/// Ergonomic constructors for the WIT `tool-result` record's two natural
/// states, removing the boilerplate of constructing
/// `ToolResult { success: true, output, error: None }` by hand.
pub trait ToolResultExt {
    fn ok(output: impl Into<String>) -> Self;
    fn err(message: impl Into<String>) -> Self;
}

impl ToolResultExt for RawToolResult {
    fn ok(output: impl Into<String>) -> Self {
        RawToolResult {
            success: true,
            output: output.into(),
            error: None,
        }
    }

    fn err(message: impl Into<String>) -> Self {
        RawToolResult {
            success: false,
            output: String::new(),
            error: Some(message.into()),
        }
    }
}

/// The trait a plugin author implements for a `tool-plugin` world. Thinner
/// than the raw generated `Guest` trait because `name`/`description`/
/// `parameters_schema` are supplied once via [`ToolMetadata`] instead of
/// three separate hand-written functions.
pub trait ToolPlugin {
    /// Static metadata for this tool, computed once at export time.
    fn metadata() -> ToolMetadata;

    /// Plugin self-identification for `plugin-info`: `(name, version)`.
    fn plugin_info() -> (&'static str, &'static str);

    /// Execute the tool with the given JSON-encoded arguments.
    fn execute(args: String) -> Result<RawToolResult, String>;
}
