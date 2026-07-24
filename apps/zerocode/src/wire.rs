//! Hand-maintained mirrors for every type that crosses the JSON-RPC
//! wire between `zerocode` and the ZeroClaw daemon.
#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Doctor result shapes ────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DoctorSeverity {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DoctorResultEntry {
    pub severity: DoctorSeverity,
    pub category: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DoctorSummary {
    pub ok: usize,
    pub warnings: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DoctorRunResult {
    pub results: Vec<DoctorResultEntry>,
    pub summary: DoctorSummary,
    /// Resolved active log persistence path from the daemon, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out_phase: Option<String>,
}

#[cfg(test)]
mod doctor_wire_tests {
    use super::*;

    #[test]
    fn doctor_run_result_round_trips_canonical_rpc_shape() {
        let canonical_json = serde_json::json!({
            "results": [
                { "severity": "ok", "category": "config", "message": "config ok" },
                { "severity": "warn", "category": "workspace", "message": "workspace warning" },
                { "severity": "error", "category": "daemon", "message": "daemon error" }
            ],
            "summary": { "ok": 1, "warnings": 1, "errors": 1 }
        });
        let mirror: DoctorRunResult = serde_json::from_value(canonical_json.clone()).unwrap();

        assert_eq!(serde_json::to_value(&mirror).unwrap(), canonical_json);
    }
}

// ── Quickstart submission shapes ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProviderChoice {
    pub provider_type: String,
    pub alias: String,
    pub model: String,
    /// Round-trip of every field the daemon described in
    /// `quickstart/fields`, keyed by `FieldDescriptor.key`. The TUI
    /// does not know what these keys mean; the daemon authored them
    /// and consumes them on the way back.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fields: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelQuickStart {
    pub channel_type: String,
    pub alias: String,
    /// Schema-keyed fields from `quickstart/fields`. ZeroCode's initialize
    /// handshake rejects daemon package-version mismatches before this wire
    /// shape can be submitted.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub fields: HashMap<String, String>,
}

#[cfg(test)]
mod quickstart_wire_tests {
    use super::*;

    #[test]
    fn channel_quickstart_wire_shape_matches_runtime_contract() {
        let channel = ChannelQuickStart {
            channel_type: "telegram".into(),
            alias: "ops".into(),
            fields: HashMap::from([("bot_token".into(), "123:ABC".into())]),
        };

        assert_eq!(
            serde_json::to_value(channel).expect("serialize channel"),
            serde_json::json!({
                "channel_type": "telegram",
                "alias": "ops",
                "fields": { "bot_token": "123:ABC" }
            })
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentIdentity {
    pub name: String,
    pub system_prompt: String,
    pub personality_file: Option<String>,
    #[serde(default)]
    pub personality_files: Vec<QuickstartPersonalityFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartPersonalityFile {
    pub filename: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartPeerGroup {
    pub name: String,
    pub channel: String,
    #[serde(default)]
    pub external_peers: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuilderSubmission {
    pub model_provider: SelectorChoice<ModelProviderChoice>,
    pub risk_profile: SelectorChoice<String>,
    pub runtime_profile: SelectorChoice<String>,
    pub memory: SelectorChoice<MemoryBackendKind>,
    pub channels: Vec<SelectorChoice<ChannelQuickStart>>,
    #[serde(default)]
    pub peer_groups: Vec<QuickstartPeerGroup>,
    pub agent: AgentIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "mode", content = "value")]
pub enum SelectorChoice<T> {
    Existing(String),
    Fresh(T),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryBackendKind {
    None,
    #[default]
    Sqlite,
    Postgres,
    Qdrant,
    Markdown,
    Lucid,
}

// ── Quickstart state / step / surface ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartState {
    pub quickstart_completed: bool,
    pub agents: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    #[serde(default)]
    pub default_runtime_profile: Option<String>,
    pub model_providers: Vec<String>,
    pub channels: Vec<String>,
    #[serde(default)]
    pub unassigned_channels: Vec<String>,
    pub storage: Vec<String>,
    #[serde(default)]
    pub model_provider_types: Vec<QuickstartTypeOption>,
    #[serde(default)]
    pub channel_types: Vec<QuickstartTypeOption>,
    #[serde(default)]
    pub risk_presets: Vec<QuickstartPresetMirror>,
    #[serde(default)]
    pub runtime_presets: Vec<QuickstartPresetMirror>,
    #[serde(default)]
    pub memory_kinds: Vec<String>,
    #[serde(default)]
    pub personality_files: Vec<String>,
}

/// Wire view of `zeroclaw_config::presets::RiskPreset` / `RuntimePreset`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartPresetMirror {
    pub preset_name: String,
    pub label: String,
    pub help: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartTypeOption {
    pub kind: String,
    pub display_name: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub default_runtime_profile: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Web,
    Tui,
    Cli,
    Test,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartStep {
    ModelProvider,
    RiskProfile,
    RuntimeProfile,
    Memory,
    Channels,
    PeerGroups,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartError {
    pub step: QuickstartStep,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AppliedAgent {
    pub alias: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldSection {
    ModelProvider,
    Channel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct FieldDescriptor {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    pub kind: PropKind,
    #[serde(default)]
    pub is_secret: bool,
    #[serde(default)]
    pub enum_variants: Option<Vec<String>>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

// ── Config explorer wire shapes ────────────────────────────────

/// Schema field-kind tag mirroring `zeroclaw_config::traits::PropKind`.
/// Carries the canonical eight variants — adding one in the schema
/// must mirror here too; `wire_drift::prop_kind_variants_round_trip`
/// fails when they diverge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PropKind {
    String,
    Bool,
    Integer,
    Float,
    Enum,
    AliasRef,
    StringArray,
    ObjectArray,
    Object,
}

impl PropKind {
    /// Wire name string, matching the canonical
    /// `zeroclaw_config::traits::PropKind::wire_name`. Used by the
    /// config explorer to render type hints.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Enum => "enum",
            Self::AliasRef => "alias_ref",
            Self::StringArray => "string_array",
            Self::ObjectArray => "object_array",
            Self::Object => "object",
        }
    }
}

/// Alias namespace for `PropKind::AliasRef` fields. Wire mirror of
/// `zeroclaw_config::traits::AliasSource`; zerocode does not depend on
/// `zeroclaw-config`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AliasSource {
    ModelProviders,
    TtsProviders,
    TranscriptionProviders,
    Channels,
    RiskProfiles,
    RuntimeProfiles,
    Agents,
    SkillBundles,
    KnowledgeBundles,
    McpBundles,
}

/// Schema-defined config tab grouping. Mirrors
/// `zeroclaw_config::traits::ConfigTab`. `Default` is `None` — the
/// "flat list, no tab bar" state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum ConfigTab {
    #[default]
    None,
    Connection,
    Advanced,
    Model,
    Behavior,
    General,
    Channels,
    Providers,
    Bundles,
    Cron,
    Tuning,
    Workspace,
    Memory,
    PeerGroups,
    Personality,
    Settings,
    Servers,
    Limits,
    Costs,
    Skills,
    Aliases,
}

impl ConfigTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Connection => "Connection",
            Self::Advanced => "Advanced",
            Self::Model => "Model",
            Self::Behavior => "Behavior",
            Self::General => "General",
            Self::Channels => "Channels",
            Self::Providers => "Providers",
            Self::Bundles => "Bundles",
            Self::Cron => "Cron",
            Self::Tuning => "Tuning",
            Self::Workspace => "Workspace",
            Self::Memory => "Memory",
            Self::PeerGroups => "Peer Groups",
            Self::Personality => "Personality",
            Self::Settings => "Settings",
            Self::Servers => "Servers",
            Self::Limits => "Limits",
            Self::Costs => "Costs",
            Self::Skills => "Skills",
            Self::Aliases => "Aliases",
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

impl std::fmt::Display for ConfigTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Single config-property descriptor returned by `config/list` and
/// `config/sections`. Mirrors `zeroclaw_config::traits::ConfigFieldEntry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFieldEntry {
    pub path: String,
    pub category: String,
    pub kind: PropKind,
    pub type_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    pub populated: bool,
    pub is_secret: bool,
    #[serde(default)]
    pub is_env_overridden: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_variants: Vec<String>,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "ConfigTab::is_none")]
    pub tab: ConfigTab,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_source: Option<AliasSource>,
}

/// Section-page shape returned by `config/sections`. Mirrors
/// `zeroclaw_config::sections::SectionShape`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SectionShape {
    DirectForm,
    OneTierAliasMap,
    TypedFamilyMap,
    BackendPicker,
}

// ── Filesystem RPC shapes ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsListDirResponse {
    pub entries: Vec<FsEntry>,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsListDirRequest {
    pub path: String,
    #[serde(default)]
    pub show_hidden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsStatResult {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub size: u64,
    pub mtime: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsStatError {
    pub path: String,
    pub code: String,
    pub message: String,
}

// ── Misc passthrough shapes ────────────────────────────────────

/// Opaque value envelope. Some RPC responses (logs subscription,
/// raw JSON-RPC notifications) carry arbitrary payloads — the TUI
/// just forwards them.
pub type RawValue = Value;

/// Mode discriminator for an outbound `elicitation/create` request.
/// Phase 1 of the rollout only emits `Form`; `Url` is on the wire
/// for future use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationMode {
    Form,
    Url,
}

/// Params for an inbound `elicitation/create` request from the
/// daemon. The TUI receives this, surfaces the form to the user,
/// and ships back an `ElicitationResponseAction` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ElicitationRequestParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub mode: ElicitationMode,
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Value,
}

/// Action discriminant the TUI returns. The daemon decodes
/// `Accept { content }` into the original choice text via
/// `zeroclaw_api::elicitation::decode_*` helpers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum ElicitationResponseAction {
    Accept {
        /// For single-select: `{"choice": "choice-<idx>"}`.
        /// For multi-select: `{"choices": ["choice-<a>", "choice-<b>", …]}`.
        content: Value,
    },
    Decline,
    Cancel,
}

/// A single option as parsed from the `oneOf` / `anyOf` schema. The
/// `const` field carries the wire id (`choice-<idx>`) and the
/// `title` field carries the human-readable label.
#[derive(Debug, Clone)]
pub struct ElicitationChoice {
    pub const_id: String,
    pub title: String,
}

/// Parsed shape of an inbound `requestedSchema` payload. Either
/// single-select (`Single`) or multi-select (`Multi`). The TUI uses
/// this to decide which modal to render. Unknown / malformed schemas
/// fall through as `None`.
#[derive(Debug, Clone)]
pub enum ElicitationShape {
    Single {
        property: String,
        choices: Vec<ElicitationChoice>,
    },
    Multi {
        property: String,
        choices: Vec<ElicitationChoice>,
        min_items: usize,
        max_items: usize,
    },
}

impl ElicitationShape {
    /// Best-effort decoder. The daemon always emits the
    /// `single_select_schema` / `multi_select_schema` shape from
    /// `zeroclaw-api`, so a return of `None` means a future schema
    /// shape we don't yet render — the TUI auto-cancels in that case.
    pub fn from_schema(schema: &Value) -> Option<Self> {
        let properties = schema.get("properties")?.as_object()?;
        let (property, prop_schema) = properties.iter().next()?;
        let property = property.clone();

        // Multi-select: `type: array` with `items.anyOf`.
        if prop_schema.get("type").and_then(Value::as_str) == Some("array") {
            let items = prop_schema.get("items")?;
            let any_of = items.get("anyOf")?.as_array()?;
            let choices = parse_choice_options(any_of);
            let min_items = prop_schema
                .get("minItems")
                .and_then(Value::as_u64)
                .unwrap_or(1) as usize;
            let max_items = prop_schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .unwrap_or(choices.len() as u64) as usize;
            return Some(Self::Multi {
                property,
                choices,
                min_items,
                max_items,
            });
        }

        // Single-select: `type: string` with `oneOf`.
        if prop_schema.get("type").and_then(Value::as_str) == Some("string") {
            let one_of = prop_schema.get("oneOf")?.as_array()?;
            let choices = parse_choice_options(one_of);
            if choices.is_empty() {
                return None;
            }
            return Some(Self::Single { property, choices });
        }

        None
    }

    /// The schema's first property name — used when building the
    /// `accept` content envelope to satisfy the issued schema.
    pub fn property(&self) -> &str {
        match self {
            Self::Single { property, .. } | Self::Multi { property, .. } => property,
        }
    }

    pub fn choices(&self) -> &[ElicitationChoice] {
        match self {
            Self::Single { choices, .. } | Self::Multi { choices, .. } => choices,
        }
    }
}

fn parse_choice_options(items: &[Value]) -> Vec<ElicitationChoice> {
    items
        .iter()
        .filter_map(|item| {
            let const_id = item.get("const")?.as_str()?.to_string();
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&const_id)
                .to_string();
            Some(ElicitationChoice { const_id, title })
        })
        .collect()
}

#[cfg(test)]
mod elicitation_wire_tests {
    use super::*;

    #[test]
    fn elicitation_response_accept_serializes_with_lowercase_action() {
        let resp = ElicitationResponseAction::Accept {
            content: serde_json::json!({ "choice": "choice-1" }),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["action"], "accept");
        assert_eq!(v["content"]["choice"], "choice-1");
    }

    #[test]
    fn elicitation_response_decline_serializes() {
        let v = serde_json::to_value(ElicitationResponseAction::Decline).unwrap();
        assert_eq!(v["action"], "decline");
    }

    #[test]
    fn elicitation_response_cancel_serializes() {
        let v = serde_json::to_value(ElicitationResponseAction::Cancel).unwrap();
        assert_eq!(v["action"], "cancel");
    }

    #[test]
    fn request_params_round_trips_canonical_shape() {
        let raw = serde_json::json!({
            "sessionId": "sess-1",
            "mode": "form",
            "message": "Pick one",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "choice": {
                        "type": "string",
                        "oneOf": [
                            { "const": "choice-0", "title": "Apple" },
                            { "const": "choice-1", "title": "Banana" }
                        ]
                    }
                },
                "required": ["choice"]
            }
        });
        let params: ElicitationRequestParams = serde_json::from_value(raw).unwrap();
        assert_eq!(params.session_id, "sess-1");
        assert_eq!(params.mode, ElicitationMode::Form);
        assert_eq!(params.message, "Pick one");
    }

    #[test]
    fn shape_decodes_single_select() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "choice": {
                    "type": "string",
                    "oneOf": [
                        { "const": "choice-0", "title": "Apple" },
                        { "const": "choice-1", "title": "Banana" }
                    ]
                }
            }
        });
        let shape = ElicitationShape::from_schema(&schema).expect("single");
        match shape {
            ElicitationShape::Single { property, choices } => {
                assert_eq!(property, "choice");
                assert_eq!(choices.len(), 2);
                assert_eq!(choices[0].const_id, "choice-0");
                assert_eq!(choices[0].title, "Apple");
                assert_eq!(choices[1].title, "Banana");
            }
            other => panic!("expected Single, got {other:?}"),
        }
    }

    #[test]
    fn shape_decodes_multi_select() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "choices": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {
                        "anyOf": [
                            { "const": "choice-0", "title": "Red" },
                            { "const": "choice-1", "title": "Green" },
                            { "const": "choice-2", "title": "Blue" }
                        ]
                    }
                }
            }
        });
        let shape = ElicitationShape::from_schema(&schema).expect("multi");
        match shape {
            ElicitationShape::Multi {
                property,
                choices,
                min_items,
                max_items,
            } => {
                assert_eq!(property, "choices");
                assert_eq!(choices.len(), 3);
                assert_eq!(min_items, 1);
                assert_eq!(max_items, 2);
                assert_eq!(choices[2].title, "Blue");
            }
            other => panic!("expected Multi, got {other:?}"),
        }
    }

    #[test]
    fn shape_returns_none_on_unknown_schema() {
        let schema = serde_json::json!({ "type": "object", "properties": {} });
        assert!(ElicitationShape::from_schema(&schema).is_none());
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    High,
    #[default]
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub status: PlanStatus,
    #[serde(default)]
    pub priority: PlanPriority,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "activeForm"
    )]
    pub active_form: Option<String>,
}

// ── TodoWrite plan wire tests ──────────────────────────────

#[cfg(test)]
mod plan_wire_tests {
    use super::*;

    #[test]
    fn plan_entry_deserializes_daemon_shape() {
        let raw = serde_json::json!({
            "content": "Analyze codebase",
            "status": "in_progress",
            "priority": "high",
            "activeForm": "Analyzing codebase"
        });
        let entry: PlanEntry = serde_json::from_value(raw).unwrap();
        assert_eq!(entry.content, "Analyze codebase");
        assert_eq!(entry.status, PlanStatus::InProgress);
        assert_eq!(entry.priority, PlanPriority::High);
        assert_eq!(entry.active_form.as_deref(), Some("Analyzing codebase"));
    }

    #[test]
    fn plan_entry_defaults_missing_optionals() {
        let raw = serde_json::json!({ "content": "x", "status": "pending" });
        let entry: PlanEntry = serde_json::from_value(raw).unwrap();
        assert_eq!(entry.priority, PlanPriority::Medium);
        assert_eq!(entry.active_form, None);
    }

    #[test]
    fn plan_entry_round_trips() {
        let entry = PlanEntry {
            content: "y".to_string(),
            status: PlanStatus::Completed,
            priority: PlanPriority::Low,
            active_form: None,
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["status"], "completed");
        assert_eq!(v["priority"], "low");
        assert!(v.get("activeForm").is_none());
        let back: PlanEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, entry);
    }
}
