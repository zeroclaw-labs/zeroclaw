//! Hand-maintained mirrors for every type that crosses the JSON-RPC
//! wire between `zerocode` and the ZeroClaw daemon.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Initialize shapes ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

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

// ── Plugin catalog shapes ──────────────────────────────────────

/// Result of `plugins/list`. Mirrors the daemon's plugin catalog body, which
/// is the same body `GET /api/plugins` serves.
///
/// Every field is required: a body missing one is malformed and fails the
/// parse instead of reading as `false` or an empty catalog. Unknown fields
/// are tolerated because the catalog contract allows additive fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginsListResult {
    /// Canonical `[plugins].enabled` config value. Configuration intent, not
    /// evidence that any plugin is loaded or healthy.
    pub plugins_enabled: bool,
    /// Whether the daemon was built with WASM plugin support. `false` is a
    /// build limitation, distinct from an empty catalog.
    pub wasm_plugins_available: bool,
    /// Configured plugin directory, before path expansion.
    pub plugins_dir: String,
    /// One row per package name, in the daemon's order.
    pub plugins: Vec<PluginCatalogEntry>,
    /// Catalog sources that could not be read, distinct from an empty catalog.
    pub issues: Vec<PluginCatalogIssue>,
}

/// One package row of the `plugins/list` body. The installed record and the
/// cached-registry record stay separate because their versions and metadata
/// can legitimately differ.
///
/// Every field is required, the optional ones included: the daemon always
/// sends `installed` and `available`, as `null` when the record is absent. A
/// row missing either key is malformed and fails the parse instead of reading
/// as "not installed" or "not in the registry".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCatalogEntry {
    pub name: String,
    // Key required; only an explicit null means no installed record.
    #[serde(deserialize_with = "Option::deserialize")]
    pub installed: Option<InstalledPluginPackage>,
    // Key required; only an explicit null means no registry record.
    #[serde(deserialize_with = "Option::deserialize")]
    pub available: Option<AvailablePluginPackage>,
}

/// Host-admitted metadata of an installed package, as the daemon's
/// `plugins/list` body reports it. Every field is required; `description` is
/// `null` when the manifest has none.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPluginPackage {
    pub version: String,
    // Key required; only an explicit null means no description.
    #[serde(deserialize_with = "Option::deserialize")]
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
}

/// Cached-registry metadata of a package, as the daemon's `plugins/list`
/// body reports it. Every field is required; `description` is `null` when
/// the registry entry has none.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailablePluginPackage {
    pub version: String,
    // Key required; only an explicit null means no description.
    #[serde(deserialize_with = "Option::deserialize")]
    pub description: Option<String>,
    pub capabilities: Vec<String>,
    /// Inert `name@version` identity. The daemon never sends registry URLs.
    pub install_source: String,
}

/// A catalog source the daemon could not read. Details stay in the daemon
/// log; the wire carries only these stable codes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginCatalogIssue {
    pub source: PluginCatalogIssueSource,
    pub code: PluginCatalogIssueCode,
}

/// Which catalog source an issue is about. A source a newer daemon adds
/// parses as `Unknown` instead of failing the whole body.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginCatalogIssueSource {
    Installed,
    Registry,
    #[serde(other)]
    Unknown,
}

/// Stable issue code. A code a newer daemon adds parses as `Unknown` instead
/// of failing the whole body.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginCatalogIssueCode {
    DiscoveryFailed,
    CacheReadFailed,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod plugin_catalog_wire_tests {
    use super::*;

    /// The daemon's canonical catalog fixture: one package installed at one
    /// version and listed in the cached registry at another.
    fn canonical_body() -> Value {
        serde_json::json!({
            "plugins_enabled": false,
            "wasm_plugins_available": true,
            "plugins_dir": "/tmp/.tmpXXXX/plugins",
            "plugins": [
                {
                    "name": "calendar",
                    "installed": {
                        "version": "0.1.0",
                        "description": "installed description",
                        "capabilities": ["tool"],
                        "permissions": ["file_read"]
                    },
                    "available": {
                        "version": "0.2.0",
                        "description": "registry description",
                        "capabilities": ["tool", "skill"],
                        "install_source": "calendar@0.2.0"
                    }
                }
            ],
            "issues": []
        })
    }

    #[test]
    fn canonical_body_round_trips() {
        let body = canonical_body();
        let parsed: PluginsListResult = serde_json::from_value(body.clone()).unwrap();

        assert!(!parsed.plugins_enabled);
        assert!(parsed.wasm_plugins_available);
        assert_eq!(parsed.plugins.len(), 1);
        let entry = &parsed.plugins[0];
        assert_eq!(entry.name, "calendar");
        let installed = entry.installed.as_ref().unwrap();
        assert_eq!(installed.version, "0.1.0");
        assert_eq!(installed.capabilities, vec!["tool"]);
        assert_eq!(installed.permissions, vec!["file_read"]);
        let available = entry.available.as_ref().unwrap();
        assert_eq!(available.version, "0.2.0");
        assert_eq!(available.capabilities, vec!["tool", "skill"]);
        assert_eq!(available.install_source, "calendar@0.2.0");
        assert_eq!(serde_json::to_value(&parsed).unwrap(), body);
    }

    #[test]
    fn registry_only_row_and_null_descriptions_parse() {
        let body = serde_json::json!({
            "plugins_enabled": true,
            "wasm_plugins_available": true,
            "plugins_dir": "~/.zeroclaw/plugins",
            "plugins": [{
                "name": "mail",
                "installed": null,
                "available": {
                    "version": "1.2.3",
                    "description": null,
                    "capabilities": ["channel"],
                    "install_source": "mail@1.2.3"
                }
            }],
            "issues": []
        });
        let parsed: PluginsListResult = serde_json::from_value(body).unwrap();
        let entry = &parsed.plugins[0];
        assert!(entry.installed.is_none());
        assert_eq!(entry.available.as_ref().unwrap().description, None);
    }

    #[test]
    fn body_without_wasm_support_parses_as_its_own_state() {
        let body = serde_json::json!({
            "plugins_enabled": true,
            "wasm_plugins_available": false,
            "plugins_dir": "~/.zeroclaw/plugins",
            "plugins": [],
            "issues": []
        });
        let parsed: PluginsListResult = serde_json::from_value(body).unwrap();
        assert!(!parsed.wasm_plugins_available);
        assert!(parsed.plugins_enabled);
        assert!(parsed.plugins.is_empty());
        assert!(parsed.issues.is_empty());
    }

    #[test]
    fn known_issue_codes_parse() {
        let body = serde_json::json!({
            "plugins_enabled": true,
            "wasm_plugins_available": true,
            "plugins_dir": "plugins",
            "plugins": [],
            "issues": [
                { "source": "installed", "code": "discovery_failed" },
                { "source": "registry", "code": "cache_read_failed" }
            ]
        });
        let parsed: PluginsListResult = serde_json::from_value(body).unwrap();
        assert_eq!(
            parsed.issues,
            vec![
                PluginCatalogIssue {
                    source: PluginCatalogIssueSource::Installed,
                    code: PluginCatalogIssueCode::DiscoveryFailed,
                },
                PluginCatalogIssue {
                    source: PluginCatalogIssueSource::Registry,
                    code: PluginCatalogIssueCode::CacheReadFailed,
                },
            ]
        );
    }

    #[test]
    fn unknown_issue_source_and_code_degrade_to_unknown() {
        let body = serde_json::json!({
            "plugins_enabled": true,
            "wasm_plugins_available": true,
            "plugins_dir": "plugins",
            "plugins": [],
            "issues": [{ "source": "mirror", "code": "signature_expired" }]
        });
        let parsed: PluginsListResult = serde_json::from_value(body).unwrap();
        assert_eq!(
            parsed.issues,
            vec![PluginCatalogIssue {
                source: PluginCatalogIssueSource::Unknown,
                code: PluginCatalogIssueCode::Unknown,
            }]
        );
    }

    #[test]
    fn unknown_extra_fields_are_tolerated() {
        let mut body = canonical_body();
        body["generated_at"] = serde_json::json!("2026-09-29T00:00:00Z");
        body["plugins"][0]["publisher"] = serde_json::json!("example");
        body["plugins"][0]["installed"]["admission"] = serde_json::json!({ "revision": 3 });
        body["plugins"][0]["available"]["homepage"] = serde_json::json!("inert");
        body["issues"] = serde_json::json!([
            { "source": "registry", "code": "cache_read_failed", "detail_id": 7 }
        ]);

        let parsed: PluginsListResult = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.plugins[0].name, "calendar");
        assert_eq!(
            parsed.issues[0].code,
            PluginCatalogIssueCode::CacheReadFailed
        );
    }

    #[test]
    fn a_missing_required_field_is_an_error() {
        for field in [
            "plugins_enabled",
            "wasm_plugins_available",
            "plugins_dir",
            "plugins",
            "issues",
        ] {
            let mut body = canonical_body();
            body.as_object_mut().unwrap().remove(field);
            let err = serde_json::from_value::<PluginsListResult>(body)
                .expect_err("a body without a required field must not parse");
            assert!(
                err.to_string().contains(field),
                "error for missing `{field}` should name it, got: {err}"
            );
        }
    }

    #[test]
    fn a_missing_nested_key_is_an_error_but_an_explicit_null_is_absent() {
        for (record, key) in [
            ("/plugins/0", "installed"),
            ("/plugins/0", "available"),
            ("/plugins/0/installed", "description"),
            ("/plugins/0/available", "description"),
        ] {
            let mut missing = canonical_body();
            let removed = missing
                .pointer_mut(record)
                .and_then(Value::as_object_mut)
                .and_then(|map| map.remove(key));
            assert!(removed.is_some(), "{record}/{key}");
            let err = serde_json::from_value::<PluginsListResult>(missing)
                .expect_err("a record without a required key must not parse");
            assert!(
                err.to_string().contains(key),
                "error for missing `{record}/{key}` should name it, got: {err}"
            );

            let mut null = canonical_body();
            null.pointer_mut(record).unwrap()[key] = Value::Null;
            serde_json::from_value::<PluginsListResult>(null)
                .unwrap_or_else(|err| panic!("an explicit null `{record}/{key}` parses: {err}"));
        }
    }
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

// ── Misc passthrough shapes ────────────────────────────────────

/// Params for an inbound `elicitation/create` request from the
/// daemon. The TUI receives this, surfaces the form to the user,
/// and responds through the JSON-RPC transport.
#[derive(Debug, Clone, Deserialize)]
pub struct ElicitationRequestParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub message: String,
    #[serde(rename = "requestedSchema")]
    pub requested_schema: Value,
}

/// A single option as parsed from the `oneOf` / `anyOf` schema. The
/// `const` field carries the wire id (`choice-<idx>`) and the
/// `title` field carries the human-readable label.
#[derive(Debug, Clone)]
pub struct ElicitationChoice {
    pub title: String,
}

/// Parsed shape of an inbound `requestedSchema` payload. Either
/// single-select (`Single`) or multi-select (`Multi`). The TUI uses
/// this to decide which modal to render. Unknown / malformed schemas
/// fall through as `None`.
#[derive(Debug, Clone)]
pub enum ElicitationShape {
    Single {
        choices: Vec<ElicitationChoice>,
    },
    Multi {
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
        let prop_schema = properties.values().next()?;

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
            return Some(Self::Single { choices });
        }

        None
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
            Some(ElicitationChoice { title })
        })
        .collect()
}

#[cfg(test)]
mod elicitation_wire_tests {
    use super::*;

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
            ElicitationShape::Single { choices } => {
                assert_eq!(choices.len(), 2);
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
                choices,
                min_items,
                max_items,
            } => {
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
