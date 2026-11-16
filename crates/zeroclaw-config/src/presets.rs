//! Quickstart submission types shared with config and its operator surfaces.

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────
// BuilderSubmission and dependent choice types
// ─────────────────────────────────────────────────────────────────────

pub use crate::multi_agent::MemoryBackendKind as MemoryChoice;

/// Model provider selection submitted by the Quickstart builder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelProviderChoice {
    /// Provider type identifier (`anthropic`, `openai`, `openrouter`,
    /// `ollama`, etc.). Used as the type segment in the TOML path.
    pub provider_type: String,
    /// User-named alias. Defaults to `"default"` in the UI; users
    /// override when stacking multiple aliases of the same provider
    /// type (e.g. `anthropic-work`, `anthropic-personal`).
    pub alias: String,
    /// Model id written to `providers.models.<type>.<alias>.model` at
    /// apply time.
    pub model: String,
    /// Round-trip of every field the daemon described in
    /// `quickstart/fields`. Surfaces echo back exactly what was
    /// emitted; the daemon writes each entry under `<prefix>.<key>`
    /// using its own schema knowledge.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub fields: std::collections::HashMap<String, String>,
}

/// One channel selection submitted by the Quickstart builder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChannelQuickStart {
    /// Channel type identifier (`cli`, `telegram`, `discord`, `web`
    /// in the FTUE-supported set).
    pub channel_type: String,
    /// User-named alias for this channel entry. Defaults to
    /// `channel_type` in the UI; users override when stacking
    /// multiple aliases of the same channel type.
    pub alias: String,
    /// Round-trip of every field the daemon described in
    /// `quickstart/fields`. Surfaces echo back exactly what was
    /// emitted; the daemon writes each entry under `<prefix>.<key>`
    /// using its own schema knowledge.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub fields: std::collections::HashMap<String, String>,
}

/// Agent identity payload from the Agent step. Personality file
/// authoring is handled by the existing `PersonalityEditor` widget;
/// Quickstart passes only the chosen `personality_file` path here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AgentIdentity {
    /// Agent alias — also the key written to `agents.<name>`. Must
    /// not collide with an existing agent alias; runtime validation
    /// rejects collisions before apply.
    pub name: String,
    /// System prompt text. Sourced from the personality template
    /// picker in the UI (`default` or `blank`); Quickstart does not
    /// pre-fill this field itself.
    pub system_prompt: String,
    /// Optional personality file path written to
    /// `agents.<name>.personality_file`. `None` ships the agent with
    /// no personality file (the existing optional pattern).
    pub personality_file: Option<String>,
    /// Staged personality file contents to write into the agent's
    /// workspace during the atomic apply. Empty list = no files
    /// written. Surfaces validate the filename against the canonical
    /// `EDITABLE_PERSONALITY_FILES` list before staging.
    #[serde(default)]
    pub personality_files: Vec<QuickstartPersonalityFile>,
}

/// One personality file staged for write during Quickstart apply.
/// The runtime writes `<workspace>/<filename>` with `content`,
/// overwriting if the path already exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct QuickstartPersonalityFile {
    /// Filename from `EDITABLE_PERSONALITY_FILES`.
    pub filename: String,
    /// File body. Subject to `MAX_FILE_CHARS` at apply time.
    pub content: String,
}

/// Complete builder submission consumed by the shared Quickstart apply path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BuilderSubmission {
    /// Model provider step submission. Always a `Create new` shape
    /// in this initial cut — `Use existing` is represented by
    /// [`SelectorChoice::Existing`] in the wrapper enum below.
    pub model_provider: SelectorChoice<ModelProviderChoice>,
    /// Risk profile preset key from the runtime Quickstart catalogue, or the alias
    /// of an existing `risk-profiles.<alias>`.
    pub risk_profile: SelectorChoice<String>,
    /// Runtime profile preset key from the runtime Quickstart catalogue, or the
    /// alias of an existing `runtime-profiles.<alias>`.
    pub runtime_profile: SelectorChoice<String>,
    /// Memory step. Either a fresh [`MemoryChoice`] or the alias of
    /// an existing `storage.<type>.<alias>` entry.
    pub memory: SelectorChoice<MemoryChoice>,
    /// Channels step. 0..N entries. Each is either a freshly-built
    /// [`ChannelQuickStart`] or the alias of an existing channel.
    /// The agent's `channels` field is auto-bound to every entry in
    /// this vec at apply time.
    pub channels: Vec<SelectorChoice<ChannelQuickStart>>,
    /// Peer groups to materialize. Each entry can reference either a
    /// staged channel from `channels` (above) or an already-configured
    /// channel ref. Empty list = no peer-group rows written.
    #[serde(default)]
    pub peer_groups: Vec<QuickstartPeerGroup>,
    /// Agent identity (always create-new — there's no reuse path).
    pub agent: AgentIdentity,
}

/// Peer-group entry staged in the Quickstart. Maps 1:1 to a
/// `[peer_groups.<name>]` table written at apply time. The `channel`
/// field carries a `<type>.<alias>` ref pointing at either a staged
/// channel from the same submission or a pre-existing one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct QuickstartPeerGroup {
    /// Map key written to `peer_groups.<name>`. Synthesized by surfaces
    /// from the channel ref so no `match` table is involved.
    pub name: String,
    /// Channel ref (`<type>.<alias>`) the peer group authorizes.
    pub channel: String,
    /// External (non-agent) peer usernames the channel should accept.
    #[serde(default)]
    pub external_peers: Vec<String>,
    /// Per-group blocklist applied to the resolved peer set.
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// Selects either an existing configured value or a fresh builder value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", tag = "mode", content = "value")]
pub enum SelectorChoice<T> {
    /// Use an already-configured alias under the corresponding
    /// section. Carries only the alias key — the runtime resolves
    /// against the live config at apply time.
    Existing(String),
    /// Create a new entry from the carried value.
    Fresh(T),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_submission_round_trips_through_json() {
        let submission = BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([(
                    "api-key".to_string(),
                    "sk-test".to_string(),
                )]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Tachi),
            channels: vec![SelectorChoice::Fresh(ChannelQuickStart {
                channel_type: "cli".into(),
                alias: "cli".into(),
                fields: std::collections::HashMap::new(),
            })],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: "my-bot".into(),
                system_prompt: "You are a helpful assistant.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        };
        let json = serde_json::to_string(&submission).expect("serialize");
        let parsed: BuilderSubmission = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, submission);
    }

    #[test]
    fn channel_quickstart_round_trips_schema_fields() {
        let channel = ChannelQuickStart {
            channel_type: "telegram".into(),
            alias: "ops".into(),
            fields: std::collections::HashMap::from([(
                "bot_token".to_string(),
                "123:ABC".to_string(),
            )]),
        };

        let value = serde_json::to_value(&channel).expect("serialize channel");
        assert_eq!(value["fields"]["bot_token"], "123:ABC");
        assert!(value.get("token").is_none());
        assert_eq!(
            serde_json::from_value::<ChannelQuickStart>(value).expect("deserialize channel"),
            channel
        );
    }

    #[test]
    fn channel_quickstart_defaults_missing_fields_to_empty() {
        let channel: ChannelQuickStart = serde_json::from_value(serde_json::json!({
            "channel_type": "cli",
            "alias": "cli"
        }))
        .expect("deserialize legacy channel without fields");

        assert!(channel.fields.is_empty());
    }
}
