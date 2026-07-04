//! Canonical event schema. OTel logs data model + ECS attribute
//! conventions, with a `zeroclaw.*` namespace for the alias-bound
//! domain attribution fields.
//!
//! On-disk JSON shape is the canonical contract — third-party tail
//! consumers parse `serde_json::Value` and walk the keys. This struct is
//! `pub(crate)` to keep external consumers off the typed surface.

use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum_macros::{EnumString, IntoStaticStr};

/// OTel severity buckets. Stored alongside `severity_number` so consumers
/// can range-compare numerically and pattern-match textually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Severity {
    // SCREAMING_SNAKE_CASE aliases so the `record!` macro can mirror
    // `tracing::Level::INFO` syntax at the call site (and so the macro
    // body's `$crate::Severity::$level` token forwarding works).
    pub const TRACE: Self = Self::Trace;
    pub const DEBUG: Self = Self::Debug;
    pub const INFO: Self = Self::Info;
    pub const WARN: Self = Self::Warn;
    pub const ERROR: Self = Self::Error;

    /// OTel severity_number for the bucket's "primary" sub-level.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Trace => 1,
            Self::Debug => 5,
            Self::Info => 9,
            Self::Warn => 13,
            Self::Error => 17,
        }
    }

    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// Convert from a `tracing::Level`.
    #[must_use]
    pub fn from_tracing_level(level: tracing::Level) -> Self {
        match level {
            tracing::Level::TRACE => Self::Trace,
            tracing::Level::DEBUG => Self::Debug,
            tracing::Level::INFO => Self::Info,
            tracing::Level::WARN => Self::Warn,
            tracing::Level::ERROR => Self::Error,
        }
    }
}

/// ECS-style event.category coarse axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum EventCategory {
    Agent,
    Channel,
    Cron,
    Memory,
    Tool,
    Provider,
    Session,
    System,
    Internal,
}

impl EventCategory {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::from_str(raw).ok()
    }
}

/// ECS event.outcome. Default unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum EventOutcome {
    Success,
    Failure,
    Unknown,
}

impl EventOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::from_str(raw).ok()
    }
}

/// ECS-style nested event descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDescriptor {
    pub category: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "is_unknown_outcome")]
    pub outcome: String,
}

fn is_unknown_outcome(s: &String) -> bool {
    s == "unknown" || s.is_empty()
}

/// Service-identifier block. Constant for the daemon's lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    pub name: String,
    pub version: String,
}

impl Default for ServiceDescriptor {
    fn default() -> Self {
        Self {
            name: "zeroclaw".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Plain alias-bound attribution fields. Adding to this list is the ONLY
/// per-field change needed — Layer/reader/gateway/UI all read this list
/// at runtime instead of hardcoding the per-field plumbing.
pub const ATTRIBUTION_FIELDS: &[&str] = &[
    "agent_alias",
    "tool",
    "session_key",
    "cron_job_id",
    "risk_profile",
    "runtime_profile",
    "memory_namespace",
    "skill_bundle",
    "knowledge_bundle",
    "mcp_bundle",
    "peer_group",
    "sop_name",
    "model",
    "embedding_provider",
    "owner_tui_id",
];

/// Composite alias-bound prefixes. Each prefix gets three on-disk keys:
/// `<prefix>` (full `<type>.<alias>`), `<prefix>_type`, `<prefix>_alias`.
/// Adding to this list propagates to every consumer the same way as
/// [`ATTRIBUTION_FIELDS`].
pub const COMPOSITE_PREFIXES: &[&str] = &[
    "channel",
    "model_provider",
    "tts_provider",
    "transcription_provider",
    "tunnel_provider",
];

/// Derive the `_type` decomposed key for a composite prefix. Single source
/// of the `_type` suffix — every reader/writer routes through this.
#[must_use]
pub fn type_field(prefix: &str) -> String {
    format!("{prefix}_type")
}

/// Derive the `_alias` decomposed key for a composite prefix. Single source
/// of the `_alias` suffix.
#[must_use]
pub fn alias_field(prefix: &str) -> String {
    format!("{prefix}_alias")
}

/// True when `name` matches a known plain attribution field, a composite
/// prefix, or a composite's decomposed `_type` / `_alias` suffix.
#[must_use]
pub fn is_attribution_field(name: &str) -> bool {
    if ATTRIBUTION_FIELDS.contains(&name) {
        return true;
    }
    for prefix in COMPOSITE_PREFIXES {
        if name == *prefix {
            return true;
        }
        if name == type_field(prefix) || name == alias_field(prefix) {
            return true;
        }
    }
    false
}

/// ZeroClaw-domain attribution. Every field is alias-bound where
/// applicable: `channel` is the `<type>.<alias>` composite, `model_provider`
/// is the `<type>.<alias>` composite, etc. Composites are stored as three
/// keys (`<prefix>`, `<prefix>_type`, `<prefix>_alias`) so filters can
/// match either coarse or precise.
///
/// The shape is a flat string map flattened into the parent on-disk JSON,
/// driven by [`ATTRIBUTION_FIELDS`] + [`COMPOSITE_PREFIXES`]. Adding a new
/// attribution key requires extending those constants — nothing else.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZeroclawAttribution {
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,

    /// Per-event duration when applicable. Kept off `fields` so JSON
    /// readers see a number, not a stringified number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl ZeroclawAttribution {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.fields.insert(key.into(), value.into());
    }

    /// Set a composite-prefixed attribution by splitting `composite` at
    /// the first `.` — populates `<prefix>`, `<prefix>_type`, and
    /// (when the dotted form is present) `<prefix>_alias` in one call.
    pub fn set_composite(&mut self, prefix: &str, composite: &str) {
        self.set(prefix.to_string(), composite.to_string());
        if let Some((ty, alias)) = composite.split_once('.') {
            self.set(type_field(prefix), ty.to_string());
            self.set(alias_field(prefix), alias.to_string());
        } else {
            self.set(type_field(prefix), composite.to_string());
        }
    }

    /// Fill any `key` absent on `self` from `other`. The flat-map shape
    /// means composite groups move as a unit naturally (all three keys
    /// merge independently, but the composite-prefix setter always
    /// writes all three together, so the parent's set is consistent).
    pub fn merge_from(&mut self, other: &Self) {
        for (k, v) in &other.fields {
            self.fields.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if self.duration_ms.is_none() {
            self.duration_ms = other.duration_ms;
        }
    }

    /// True when every plain field in [`ATTRIBUTION_FIELDS`] and every
    /// composite in [`COMPOSITE_PREFIXES`] has been populated — the
    /// span-walk uses this as a "no point looking further up" check.
    #[must_use]
    pub fn is_fully_populated(&self) -> bool {
        ATTRIBUTION_FIELDS
            .iter()
            .all(|k| self.fields.contains_key(*k))
            && COMPOSITE_PREFIXES
                .iter()
                .all(|p| self.fields.contains_key(*p))
    }
}

/// One row in the canonical log stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    /// Persistent event id. UUID v4.
    pub id: String,

    /// RFC 3339 UTC timestamp with milliseconds. Keyed `@timestamp` to
    /// match ECS conventions; consumers (and our paginated reader) sort
    /// by this lexicographically, which works because RFC 3339 is sortable
    /// as a string.
    #[serde(rename = "@timestamp")]
    pub timestamp: String,

    pub severity_number: u8,
    pub severity_text: String,

    pub event: EventDescriptor,

    #[serde(default)]
    pub service: ServiceDescriptor,

    /// Per-turn trace identifier so multiple events from one agent
    /// turn group together in the UI. Populated by the `LogCaptureLayer`,
    /// which promotes it from `attributes.trace_id`, set at the call site
    /// via `record!(.. with_attrs(json!({"trace_id": ..})))` or inherited
    /// from a `scope!(trace_id: ..)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    /// Sub-span within a turn (e.g. one tool call inside a multi-tool
    /// iteration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,

    /// All the alias-bound attribution fields live here.
    #[serde(default)]
    pub zeroclaw: ZeroclawAttribution,

    /// Human-readable short message. The structured fields above carry the
    /// machine-readable detail; `message` is what a terminal-formatter
    /// prints as the line body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Free-form structured payload. Per-action contributors put extra
    /// data here (tokens used, iteration counter, tool input/output
    /// payloads when `log_tool_io` is enabled, anyhow error chain when
    /// the event is an error, …).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub attributes: Value,

    /// Schema version. `2` = this struct. Older files containing version-1
    /// rows get migrated in place at daemon startup.
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
}

fn default_schema_version() -> u8 {
    LogEvent::SCHEMA_VERSION
}

impl LogEvent {
    pub const SCHEMA_VERSION: u8 = 2;

    /// Build a fresh event with the given level + action + category.
    /// Caller fills in attribution and message before emission.
    #[must_use]
    pub fn new(severity: Severity, action: &str, category: EventCategory) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            severity_number: severity.number(),
            severity_text: severity.text().to_string(),
            event: EventDescriptor {
                category: category.as_str().to_string(),
                action: action.to_string(),
                outcome: EventOutcome::Unknown.as_str().to_string(),
            },
            service: ServiceDescriptor::default(),
            trace_id: None,
            span_id: None,
            zeroclaw: ZeroclawAttribution::default(),
            message: None,
            attributes: Value::Null,
            schema_version: LogEvent::SCHEMA_VERSION,
        }
    }

    pub fn set_outcome(&mut self, outcome: EventOutcome) {
        self.event.outcome = outcome.as_str().to_string();
    }
}

/// Lookup helper used by callers that already have an OTel-style severity
/// number and want the text bucket.
#[must_use]
pub fn severity_text_from_number(n: u8) -> &'static str {
    match n {
        0..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        _ => "FATAL",
    }
}

#[must_use]
pub fn severity_text_from_tracing_level(level: tracing::Level) -> &'static str {
    Severity::from_tracing_level(level).text()
}

// ---------------------------------------------------------------------------
// Call-site Event surface
// ---------------------------------------------------------------------------

/// Closed `event.action` taxonomy. Adding a verb requires extending
/// this enum — no `Other(&str)` escape hatch on purpose. The snake_case
/// form of each variant is the on-disk `event.action` string, derived
/// via `strum::IntoStaticStr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Action {
    Start,
    Complete,
    Fail,
    Cancel,
    Skip,
    Timeout,
    Retry,
    Inbound,
    Outbound,
    Send,
    Receive,
    Connect,
    Disconnect,
    Reconnect,
    Spawn,
    Kill,
    Tick,
    Trigger,
    Schedule,
    Approve,
    Reject,
    Defer,
    Read,
    Write,
    Delete,
    List,
    Query,
    Invoke,
    Dispatch,
    Resolve,
    Register,
    Unregister,
    Load,
    Save,
    Migrate,
    Validate,
    Note,
}

impl Action {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// One emission's call-site descriptor. Built by the `record!` macro
/// from the Event constructor + builder calls and consumed by the layer.
/// Everything is by-value; the macro takes `&Event` to keep call sites
/// non-moving.
#[derive(Debug, Clone)]
pub struct Event {
    pub name: &'static str,
    pub action: Action,
    pub category: Option<EventCategory>,
    pub outcome: EventOutcome,
    pub duration_ms: Option<u64>,
    pub attrs: Option<Value>,
}

impl Event {
    #[must_use]
    pub fn new(name: &'static str, action: Action) -> Self {
        Self {
            name,
            action,
            category: None,
            outcome: EventOutcome::Unknown,
            duration_ms: None,
            attrs: None,
        }
    }

    #[must_use]
    pub fn with_category(mut self, category: EventCategory) -> Self {
        self.category = Some(category);
        self
    }

    #[must_use]
    pub fn with_outcome(mut self, outcome: EventOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    #[must_use]
    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    #[must_use]
    pub fn with_attrs(mut self, attrs: Value) -> Self {
        self.attrs = Some(attrs);
        self
    }

    #[must_use]
    pub fn category_str(&self) -> &'static str {
        self.category.map_or("", EventCategory::as_str)
    }

    #[must_use]
    pub fn outcome_str(&self) -> &'static str {
        self.outcome.as_str()
    }

    /// JSON-encode the attrs payload as a string for tracing::event!
    /// transport. Layer parses back to `Value`.
    #[must_use]
    pub fn attrs_str(&self) -> String {
        match &self.attrs {
            Some(v) => serde_json::to_string(v).unwrap_or_default(),
            None => String::new(),
        }
    }

    #[must_use]
    pub fn duration_ms_or_zero(&self) -> u64 {
        self.duration_ms.unwrap_or(0)
    }

    #[must_use]
    pub fn has_duration(&self) -> bool {
        self.duration_ms.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_round_trip_through_tracing() {
        for (level, severity) in [
            (tracing::Level::TRACE, Severity::Trace),
            (tracing::Level::DEBUG, Severity::Debug),
            (tracing::Level::INFO, Severity::Info),
            (tracing::Level::WARN, Severity::Warn),
            (tracing::Level::ERROR, Severity::Error),
        ] {
            assert_eq!(Severity::from_tracing_level(level), severity);
        }
    }

    #[test]
    fn severity_text_buckets_match_number() {
        assert_eq!(severity_text_from_number(1), "TRACE");
        assert_eq!(severity_text_from_number(5), "DEBUG");
        assert_eq!(severity_text_from_number(9), "INFO");
        assert_eq!(severity_text_from_number(13), "WARN");
        assert_eq!(severity_text_from_number(17), "ERROR");
        assert_eq!(severity_text_from_number(22), "FATAL");
    }

    #[test]
    fn set_composite_splits_channel() {
        let mut attribution = ZeroclawAttribution::default();
        attribution.set_composite("channel", "discord.clamps");
        assert_eq!(attribution.get("channel"), Some("discord.clamps"));
        assert_eq!(attribution.get("channel_type"), Some("discord"));
        assert_eq!(attribution.get("channel_alias"), Some("clamps"));
    }

    #[test]
    fn set_composite_bare_type() {
        let mut attribution = ZeroclawAttribution::default();
        attribution.set_composite("channel", "webhook");
        assert_eq!(attribution.get("channel_type"), Some("webhook"));
        assert!(attribution.get("channel_alias").is_none());
    }

    #[test]
    fn set_composite_splits_model_provider() {
        let mut attribution = ZeroclawAttribution::default();
        attribution.set_composite("model_provider", "anthropic.clamps");
        assert_eq!(attribution.get("model_provider"), Some("anthropic.clamps"));
        assert_eq!(attribution.get("model_provider_type"), Some("anthropic"));
        assert_eq!(attribution.get("model_provider_alias"), Some("clamps"));
    }

    #[test]
    fn merge_from_fills_missing_only() {
        let mut child = ZeroclawAttribution::default();
        child.set("agent_alias", "clamps");
        let mut parent = ZeroclawAttribution::default();
        parent.set("agent_alias", "glados");
        parent.set("risk_profile", "strict");
        child.merge_from(&parent);
        assert_eq!(child.get("agent_alias"), Some("clamps"));
        assert_eq!(child.get("risk_profile"), Some("strict"));
    }

    #[test]
    fn is_attribution_field_recognises_composites() {
        assert!(is_attribution_field("channel"));
        assert!(is_attribution_field("channel_type"));
        assert!(is_attribution_field("channel_alias"));
        assert!(is_attribution_field("model_provider_alias"));
        assert!(is_attribution_field("agent_alias"));
        assert!(!is_attribution_field("not_a_real_field"));
    }

    #[test]
    fn event_serializes_with_at_timestamp_key() {
        let event = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        let serialized = serde_json::to_value(&event).unwrap();
        assert!(serialized.get("@timestamp").is_some());
        assert!(serialized.get("timestamp").is_none());
        assert_eq!(serialized["severity_text"], "INFO");
        assert_eq!(serialized["severity_number"], 9);
        assert_eq!(serialized["event"]["category"], "agent");
        assert_eq!(serialized["event"]["action"], "test");
        assert_eq!(serialized["schema_version"], LogEvent::SCHEMA_VERSION);
    }

    #[test]
    fn unknown_outcome_omitted_from_serialization() {
        let event = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        let serialized = serde_json::to_value(&event).unwrap();
        assert!(serialized["event"].get("outcome").is_none());
    }
}
