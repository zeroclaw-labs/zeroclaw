//! Hand-maintained mirrors for every type that crosses the JSON-RPC
//! wire between `zerocode` and the ZeroClaw daemon.
//!
//! These mirrors exist so `apps/zerocode/Cargo.toml` carries zero
//! workspace dependencies in `[dependencies]`. The TUI talks JSON-RPC
//! to whatever daemon is at the configured address; the wire shape is
//! the contract, not a shared Rust type.
//!
//! Drift between these mirrors and the canonical workspace types is
//! caught by `apps/zerocode/tests/wire_drift.rs`, which pulls the
//! canonical types via `[dev-dependencies]` and asserts JSON-byte
//! equality after a serialize / deserialize / re-serialize cycle.
//!
//! Some mirrors here are unused by the running TUI today — they
//! exist to lock the wire contract for every type the daemon emits
//! so that adding a new use-site in the TUI doesn't have to re-derive
//! the shape from scratch and risk drift.
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
}

#[cfg(test)]
mod doctor_wire_tests {
    use super::*;
    use zeroclaw_runtime::{
        doctor::{DiagResult, Severity},
        rpc::types::{
            DoctorRunResult as RuntimeDoctorRunResult, DoctorSummary as RuntimeDoctorSummary,
        },
    };

    #[test]
    fn doctor_run_result_round_trips_canonical_rpc_shape() {
        let canonical = RuntimeDoctorRunResult {
            results: vec![
                DiagResult {
                    severity: Severity::Ok,
                    category: "config".to_string(),
                    message: "config ok".to_string(),
                },
                DiagResult {
                    severity: Severity::Warn,
                    category: "workspace".to_string(),
                    message: "workspace warning".to_string(),
                },
                DiagResult {
                    severity: Severity::Error,
                    category: "daemon".to_string(),
                    message: "daemon error".to_string(),
                },
            ],
            summary: RuntimeDoctorSummary {
                ok: 1,
                warnings: 1,
                errors: 1,
            },
        };

        let canonical_json = serde_json::to_value(&canonical).unwrap();
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
    pub token: Option<String>,
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
