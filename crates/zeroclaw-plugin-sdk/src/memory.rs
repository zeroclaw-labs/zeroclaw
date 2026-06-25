//! Ergonomics for the `memory-plugin` world. Enable the `memory` feature
//! and implement [`MemoryPlugin`], then call [`crate::export_memory!`]
//! once with your type.

use crate::bindings::memory::zeroclaw::plugin::{logging, plugin_config};

pub use crate::bindings::memory::exports::zeroclaw::plugin::memory::{
    AgentFilter, ExportFilter, MemoryCapabilities, MemoryCategory, MemoryEntry, ProceduralMessage,
};
pub use logging::{LogLevel, PluginAction, PluginEvent, PluginOutcome};

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

/// The trait a plugin author implements for a `memory-plugin` world.
///
/// Required methods must be implemented. Capability-gated methods have
/// default bodies matching the stub values `wit/v0/memory.wit` documents
/// for each one verbatim (e.g. `reindex` -> `Ok(0)`,
/// `get_for_agent` -> `Err("not-supported")`) — without these defaults, a
/// plugin implementing only the required subset would have to hand-write
/// 11 stub functions. [`MemoryPlugin::get_memory_capabilities`] tells the
/// host which of these are real vs. defaulted; the host only calls a
/// capability-gated method when its flag is set.
pub trait MemoryPlugin {
    fn plugin_info() -> (&'static str, &'static str);
    fn name() -> String;
    fn get_memory_capabilities() -> MemoryCapabilities;

    fn store_entry(
        key: String,
        content: String,
        category: MemoryCategory,
        session_id: Option<String>,
    ) -> Result<(), String>;

    fn recall(
        query: String,
        limit: u64,
        session_id: Option<String>,
        since: Option<String>,
        until: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String>;

    fn get(key: String) -> Result<Option<MemoryEntry>, String>;

    fn list_entries(
        category: Option<MemoryCategory>,
        session_id: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String>;

    fn forget(key: String) -> Result<bool, String>;

    fn forget_for_agent(key: String, agent_id: String) -> Result<bool, String>;

    fn count() -> Result<u64, String>;

    fn health_check() -> bool;

    fn store_with_agent(
        key: String,
        content: String,
        category: MemoryCategory,
        session_id: Option<String>,
        namespace: Option<String>,
        importance: Option<f64>,
        agent_id: Option<String>,
    ) -> Result<(), String>;

    fn recall_for_agents(
        agents: AgentFilter,
        query: String,
        limit: u64,
        session_id: Option<String>,
        since: Option<String>,
        until: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String>;

    // ── Capability-gated, defaulted per wit/v0/memory.wit's documented stub values ──

    fn get_for_agent(_key: String, _agent_id: String) -> Result<Option<MemoryEntry>, String> {
        Err("not-supported".to_string())
    }

    fn purge_namespace(_namespace: String) -> Result<u64, String> {
        Err("not-supported".to_string())
    }

    fn purge_session(_session_id: String) -> Result<u64, String> {
        Err("not-supported".to_string())
    }

    fn purge_session_for_agent(_session_id: String, _agent_id: String) -> Result<u64, String> {
        Err("not-supported".to_string())
    }

    fn purge_agent(_agent_alias: String) -> Result<u64, String> {
        Err("not-supported".to_string())
    }

    fn reindex() -> Result<u64, String> {
        Ok(0)
    }

    fn store_procedural(
        _messages: Vec<ProceduralMessage>,
        _session_id: Option<String>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn ensure_agent_uuid(alias: String) -> Result<String, String> {
        Ok(alias)
    }

    fn recall_namespaced(
        _namespace: String,
        _query: String,
        _limit: u64,
        _session_id: Option<String>,
        _since: Option<String>,
        _until: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String> {
        Err("not-supported".to_string())
    }

    fn export_entries(_filter: ExportFilter) -> Result<Vec<MemoryEntry>, String> {
        Err("not-supported".to_string())
    }

    fn store_with_metadata(
        _key: String,
        _content: String,
        _category: MemoryCategory,
        _session_id: Option<String>,
        _namespace: Option<String>,
        _importance: Option<f64>,
    ) -> Result<(), String> {
        Err("not-supported".to_string())
    }
}
