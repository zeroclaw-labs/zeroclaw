//! Quickstart preset tables and submission shape.
//!
//! Two preset tables — [`RISK_PRESETS`] and [`RUNTIME_PRESETS`] — give
//! the Quickstart UI a fixed-shape menu of named, opinionated profile
//! defaults the user can pick from. Each preset carries:
//!
//! - `preset_name`  — the alias key written to config when picked
//!   (`risk-profiles.<preset_name>` / `runtime-profiles.<preset_name>`).
//!   Never `default`. The preset is canonical: picking it again
//!   overwrites the alias of the same name with the preset's struct
//!   values.
//! - `label` / `help` — the strings the UI renders.
//! - `values` — a struct literal of [`RiskProfileConfig`] /
//!   [`RuntimeProfileConfig`] field values. The Quickstart writes
//!   these verbatim into the corresponding config table on apply.
//!
//! Adding or removing a preset is one row in the `risk_presets!` /
//! `runtime_presets!` table below; every consumer dispatches off
//! `&'static [RiskPreset]` / `&'static [RuntimePreset]` so drift is
//! impossible.
//!
//! [`BuilderSubmission`] is the single payload shape both surfaces
//! (web gateway HTTP route, zerocode RPC route) and the CLI build and
//! hand to `zeroclaw-runtime`'s apply path. The runtime validates and
//! writes atomically. There is exactly one type, one validator, one
//! apply function — surface code never assembles config directly.

use serde::{Deserialize, Serialize};

use crate::autonomy::AutonomyLevel;
use crate::autonomy::DelegationPolicy;
use crate::policy::{default_allowed_commands, default_forbidden_paths};
use crate::schema::{RiskProfileConfig, RuntimeProfileConfig};

// ─────────────────────────────────────────────────────────────────────
// Risk presets
// ─────────────────────────────────────────────────────────────────────

/// One row in the Risk preset table. The Quickstart UI renders the
/// `label`, the runtime writes `values` to
/// `risk-profiles.<preset_name>` on apply.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RiskPreset {
    /// Alias key written to `risk-profiles.<preset_name>`. Doubles as
    /// the stable wire identifier (`BuilderSubmission.risk_preset`).
    pub preset_name: &'static str,
    /// Short label rendered in the picker UI.
    pub label: &'static str,
    /// One-line help blurb rendered next to the label.
    pub help: &'static str,
    /// Factory that produces the [`RiskProfileConfig`] this preset
    /// installs. A function (not a const value) because
    /// `RiskProfileConfig` has owned `Vec<String>` fields that cannot
    /// live in a `const`.
    #[serde(skip)]
    pub values: fn() -> RiskProfileConfig,
}

/// Canonical Risk preset table. Order is the order the picker
/// renders. Add or remove a preset by editing one row here; every
/// consumer reads from the slice so nothing else has to change.
pub const RISK_PRESETS: &[RiskPreset] = &[
    RiskPreset {
        preset_name: "locked_down",
        label: "Locked Down",
        help: "Tightest defaults. Workspace-only filesystem access, approval \
               required for medium and high risk, no shell environment passthrough.",
        values: locked_down_risk,
    },
    RiskPreset {
        preset_name: "balanced",
        label: "Balanced",
        help: "Sensible day-to-day defaults. Workspace-scoped with approval \
               gates on risky operations. Recommended for most users.",
        values: balanced_risk,
    },
    RiskPreset {
        preset_name: "yolo",
        label: "YOLO",
        help: "Full autonomy. No approval gates, no command denylist, no \
               workspace scoping. Only pick this if you know what you're \
               doing on a machine you don't mind breaking.",
        values: yolo_risk,
    },
];

/// Look up a Risk preset by its `preset_name`. Returns `None` for
/// unknown keys.
#[must_use]
pub fn risk_preset(preset_name: &str) -> Option<&'static RiskPreset> {
    RISK_PRESETS.iter().find(|p| p.preset_name == preset_name)
}

fn locked_down_risk() -> RiskProfileConfig {
    RiskProfileConfig {
        level: AutonomyLevel::Supervised,
        workspace_only: true,
        allowed_commands: default_allowed_commands(),
        forbidden_paths: default_forbidden_paths(),
        require_approval_for_medium_risk: true,
        block_high_risk_commands: true,
        shell_env_passthrough: vec![],
        auto_approve: vec![],
        always_ask: vec![],
        allowed_roots: vec![],
        delegation_policy: DelegationPolicy::default(),
        allowed_tools: vec![],
        excluded_tools: vec![],
        sandbox_enabled: Some(true),
        sandbox_backend: None,
        firejail_args: vec![],
    }
}

fn balanced_risk() -> RiskProfileConfig {
    // Schema default is already the Balanced shape (Supervised,
    // workspace_only=true, medium-risk approval). Use it directly so
    // the preset can't drift away from the schema default by accident.
    RiskProfileConfig::default()
}

fn yolo_risk() -> RiskProfileConfig {
    RiskProfileConfig {
        level: AutonomyLevel::Full,
        workspace_only: false,
        // YOLO means "no command denylist" — but an EMPTY allowlist is
        // deny-by-default (`is_command_allowed` rejects any command not
        // matched by an entry), so `vec![]` blocks every shell command.
        // The `*` wildcard + `block_high_risk_commands: false` is what
        // actually grants unrestricted execution (the trusted-env path in
        // `is_command_allowed`).
        allowed_commands: vec!["*".to_string()],
        forbidden_paths: vec![],
        require_approval_for_medium_risk: false,
        block_high_risk_commands: false,
        shell_env_passthrough: vec![],
        auto_approve: vec![],
        always_ask: vec![],
        allowed_roots: vec![],
        delegation_policy: DelegationPolicy::default(),
        allowed_tools: vec![],
        excluded_tools: vec![],
        sandbox_enabled: Some(false),
        sandbox_backend: None,
        firejail_args: vec![],
    }
}

// ─────────────────────────────────────────────────────────────────────
// Runtime presets
// ─────────────────────────────────────────────────────────────────────

/// One row in the Runtime preset table. Same shape and contract as
/// [`RiskPreset`] — see its docs for the per-field semantics.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RuntimePreset {
    /// Alias key written to `runtime-profiles.<preset_name>`. Doubles
    /// as the stable wire identifier (`BuilderSubmission.runtime_preset`).
    pub preset_name: &'static str,
    /// Short label rendered in the picker UI.
    pub label: &'static str,
    /// One-line help blurb rendered next to the label.
    pub help: &'static str,
    /// Factory that produces the [`RuntimeProfileConfig`] this preset
    /// installs.
    #[serde(skip)]
    pub values: fn() -> RuntimeProfileConfig,
}

/// Canonical Runtime preset table. See [`RISK_PRESETS`] for the
/// ordering / consumer contract.
pub const RUNTIME_PRESETS: &[RuntimePreset] = &[
    RuntimePreset {
        preset_name: "tight",
        label: "Tight",
        help: "Small budgets and short timeouts. Good for cheap models, \
               metered API keys, or tight feedback loops where you want \
               the agent to stop and ask early rather than burn budget.",
        values: tight_runtime,
    },
    RuntimePreset {
        preset_name: "balanced",
        label: "Balanced",
        help: "Middle-of-the-road operational defaults. Suits most users \
               most of the time.",
        values: balanced_runtime,
    },
    RuntimePreset {
        preset_name: "unbounded",
        label: "Unbounded",
        help: "Wide-open budgets and long timeouts. Pick this when you're \
               actively driving the agent through a hard task and don't \
               want it to throttle.",
        values: unbounded_runtime,
    },
];

/// Look up a Runtime preset by its `preset_name`. Returns `None` for
/// unknown keys.
#[must_use]
pub fn runtime_preset(preset_name: &str) -> Option<&'static RuntimePreset> {
    RUNTIME_PRESETS
        .iter()
        .find(|p| p.preset_name == preset_name)
}

fn tight_runtime() -> RuntimeProfileConfig {
    RuntimeProfileConfig {
        agentic: false,
        max_tool_iterations: 10,
        max_actions_per_hour: 10,
        max_cost_per_day_cents: 100,
        shell_timeout_secs: 30,
        max_delegation_depth: 1,
        delegation_timeout_secs: Some(60),
        agentic_timeout_secs: Some(120),
        max_history_messages: Some(20),
        max_context_tokens: Some(8_000),
        compact_context: Some(true),
        parallel_tools: Some(false),
        tool_dispatcher: None,
        tool_call_dedup_exempt: vec![],
        max_system_prompt_chars: Some(4_000),
        max_tool_result_chars: Some(8_000),
        keep_tool_context_turns: Some(2),
        memory_recall_limit: Some(3),
        ..RuntimeProfileConfig::default()
    }
}

fn balanced_runtime() -> RuntimeProfileConfig {
    // Schema default is already the Balanced shape. Use it directly so
    // the preset can't drift from the schema default.
    RuntimeProfileConfig::default()
}

fn unbounded_runtime() -> RuntimeProfileConfig {
    RuntimeProfileConfig {
        agentic: true,
        max_tool_iterations: 100,
        // `0` is NOT "unlimited" for these budgets — the per-sender rate
        // tracker treats a max of 0 as *exhausted* (see
        // `PerSenderTracker::is_exhausted` / `rate_limit_zero_blocks_everything`),
        // so an `unbounded` agent set to 0 has every action rejected. Use the
        // type max for an effectively-unlimited budget instead.
        max_actions_per_hour: u32::MAX,
        max_cost_per_day_cents: u32::MAX,
        shell_timeout_secs: 600,
        max_delegation_depth: 8,
        delegation_timeout_secs: Some(900),
        agentic_timeout_secs: Some(1_800),
        max_history_messages: Some(200),
        max_context_tokens: Some(128_000),
        compact_context: Some(false),
        parallel_tools: Some(true),
        tool_dispatcher: None,
        tool_call_dedup_exempt: vec![],
        max_system_prompt_chars: Some(64_000),
        max_tool_result_chars: Some(64_000),
        keep_tool_context_turns: Some(8),
        memory_recall_limit: Some(10),
        ..RuntimeProfileConfig::default()
    }
}

// ─────────────────────────────────────────────────────────────────────
// BuilderSubmission and dependent choice types
// ─────────────────────────────────────────────────────────────────────

/// Choice for the Memory step. Re-exports the schema's canonical
/// `MemoryBackendKind` so Quickstart never re-defines the list of
/// memory backends — adding a backend to
/// `zeroclaw_config::multi_agent::MemoryBackendKind` lights up in
/// every Quickstart surface automatically.
pub use crate::multi_agent::MemoryBackendKind as MemoryChoice;

/// Model provider widget submission. The Quickstart UI surfaces only
/// the "greatest hits" fields an agent literally cannot start
/// without; everything else (retry policy, rate limits, custom
/// headers) lives in the post-Quickstart config editor.
///
/// `provider_type` is the type key written to
/// `providers.models.<provider_type>.<alias>`. The exact set of
/// recognised type strings tracks the existing
/// `providers::ProviderKind`; Quickstart validates the chosen value
/// at apply time via the runtime entry point.
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

/// Single-channel entry submitted by the Channels widget. The
/// Channels selector renders a `Vec<ChannelQuickStart>`; Quickstart
/// writes one `channels.<channel_type>.<alias>` block per entry.
///
/// Channels are optional: an empty `Vec` is a valid Quickstart
/// submission (the agent will only be reachable via
/// `zeroclaw agent <alias>` from the CLI).
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
    /// Bot token / shared secret if the channel needs one
    /// (Telegram, Discord). `None` for channels that don't.
    pub token: Option<String>,
    /// HTTP port for webhook channel types. `None` for channels that
    /// don't need one. The CLI adapter validates the raw port string
    /// as `u16` and rejects invalid input.
    #[serde(default)]
    pub port: Option<u16>,
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

/// The complete Quickstart submission both surfaces hand to
/// `zeroclaw-runtime::quickstart::apply` (and pre-validate via
/// `validate_only`). Single source of truth; assembling config
/// outside this type is a layering bug.
///
/// Every field's `*_preset` / choice value is the user's resolved
/// selection — the runtime translates preset keys into struct
/// values via [`risk_preset`] / [`runtime_preset`] and looks up
/// existing aliases against the live config when the UI submitted
/// "use existing" rather than a fresh choice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct BuilderSubmission {
    /// Model provider step submission. Always a `Create new` shape
    /// in this initial cut — `Use existing` is represented by
    /// [`SelectorChoice::Existing`] in the wrapper enum below.
    pub model_provider: SelectorChoice<ModelProviderChoice>,
    /// Risk profile preset key from [`RISK_PRESETS`], or the alias
    /// of an existing `risk-profiles.<alias>`.
    pub risk_profile: SelectorChoice<String>,
    /// Runtime profile preset key from [`RUNTIME_PRESETS`], or the
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

/// Dual-mode selector outcome. Every Quickstart selector lets the
/// user either pick an existing configured alias or create a fresh
/// one; this enum carries which path was taken so the runtime apply
/// path can branch on `Existing` (record an alias ref only, no
/// writes to that section) vs `Fresh` (write a new entry).
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

    /// Every preset's `preset_name` must be unique within its table —
    /// the alias is also the lookup key, so duplicates would shadow
    /// each other silently.
    #[test]
    fn risk_preset_names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for p in RISK_PRESETS {
            assert!(
                seen.insert(p.preset_name),
                "duplicate risk preset_name: {}",
                p.preset_name
            );
        }
    }

    #[test]
    fn runtime_preset_names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for p in RUNTIME_PRESETS {
            assert!(
                seen.insert(p.preset_name),
                "duplicate runtime preset_name: {}",
                p.preset_name
            );
        }
    }

    /// `risk_preset` / `runtime_preset` lookup round-trip — picking
    /// by `preset_name` must find the same row that's in the slice.
    #[test]
    fn risk_preset_lookup_round_trips() {
        for p in RISK_PRESETS {
            let found = risk_preset(p.preset_name).expect("preset present");
            assert_eq!(found.preset_name, p.preset_name);
            assert_eq!(found.label, p.label);
        }
        assert!(risk_preset("not-a-real-preset").is_none());
    }

    #[test]
    fn runtime_preset_lookup_round_trips() {
        for p in RUNTIME_PRESETS {
            let found = runtime_preset(p.preset_name).expect("preset present");
            assert_eq!(found.preset_name, p.preset_name);
            assert_eq!(found.label, p.label);
        }
        assert!(runtime_preset("not-a-real-preset").is_none());
    }

    /// No preset is allowed to use `default` as its alias.
    #[test]
    fn no_preset_uses_default_alias() {
        for p in RISK_PRESETS {
            assert_ne!(
                p.preset_name, "default",
                "risk preset alias must never be `default`",
            );
        }
        for p in RUNTIME_PRESETS {
            assert_ne!(
                p.preset_name, "default",
                "runtime preset alias must never be `default`",
            );
        }
    }

    #[test]
    fn preset_names_are_valid_alias_keys() {
        for p in RISK_PRESETS {
            crate::helpers::validate_alias_key(p.preset_name).unwrap_or_else(|e| {
                panic!(
                    "risk preset_name `{}` is not a valid alias key: {e}",
                    p.preset_name
                )
            });
        }
        for p in RUNTIME_PRESETS {
            crate::helpers::validate_alias_key(p.preset_name).unwrap_or_else(|e| {
                panic!(
                    "runtime preset_name `{}` is not a valid alias key: {e}",
                    p.preset_name
                )
            });
        }
    }

    /// The `Balanced` preset must equal the schema's `Default::default()` —
    /// that's the contract that lets us call `RiskProfileConfig::default()`
    /// / `RuntimeProfileConfig::default()` for the Balanced factory
    /// instead of duplicating field literals.
    #[test]
    fn balanced_risk_matches_schema_default() {
        let preset = risk_preset("balanced").unwrap();
        let preset_values = (preset.values)();
        let schema_default = RiskProfileConfig::default();
        // Compare via Debug since RiskProfileConfig doesn't derive PartialEq.
        assert_eq!(format!("{preset_values:?}"), format!("{schema_default:?}"),);
    }

    #[test]
    fn balanced_runtime_matches_schema_default() {
        let preset = runtime_preset("balanced").unwrap();
        let preset_values = (preset.values)();
        let schema_default = RuntimeProfileConfig::default();
        assert_eq!(format!("{preset_values:?}"), format!("{schema_default:?}"),);
    }

    /// Regression: the `unbounded` preset must NOT zero out the action
    /// budget. A `max_actions_per_hour` of 0 is a hard zero budget (the
    /// per-sender tracker treats 0 as always exhausted), so an agent on
    /// the `unbounded` profile previously had every tool call rejected
    /// with "max 0 actions per hour". Assert the budget is non-zero and
    /// that a policy carrying it actually permits an action.
    #[test]
    fn unbounded_runtime_does_not_block_all_actions() {
        let preset = runtime_preset("unbounded").unwrap();
        let values = (preset.values)();
        assert_ne!(
            values.max_actions_per_hour, 0,
            "unbounded must not use 0 — 0 means a hard zero action budget, not unlimited",
        );
        let policy = crate::policy::SecurityPolicy {
            max_actions_per_hour: values.max_actions_per_hour,
            ..crate::policy::SecurityPolicy::default()
        };
        assert!(
            policy.record_action(),
            "an unbounded-profile agent must be allowed to take actions",
        );
    }

    /// Regression: the `yolo` risk preset must actually permit shell
    /// commands. `allowed_commands` is deny-by-default — an empty list
    /// matches nothing, so a `yolo` agent (whose whole point is "no
    /// command denylist, full autonomy") previously had every shell
    /// command rejected with "Command not allowed by security policy".
    /// The preset must carry the `*` wildcard so unrestricted execution
    /// is actually granted.
    #[test]
    fn yolo_risk_allows_shell_commands() {
        let preset = risk_preset("yolo").unwrap();
        let values = (preset.values)();
        let policy = crate::policy::SecurityPolicy {
            autonomy: values.level,
            allowed_commands: values.allowed_commands.clone(),
            block_high_risk_commands: values.block_high_risk_commands,
            ..crate::policy::SecurityPolicy::default()
        };
        for cmd in ["ls", "pwd", "cat README.md", "rm -rf node_modules"] {
            assert!(
                policy.is_command_allowed(cmd),
                "yolo profile must allow `{cmd}` — it grants full autonomy with no denylist",
            );
        }
    }

    /// `BuilderSubmission` and its dependent types must round-trip
    /// through serde — both surfaces serialize the same shape, and
    /// the drift test in commit 4 will rely on this.
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
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![SelectorChoice::Fresh(ChannelQuickStart {
                channel_type: "cli".into(),
                alias: "cli".into(),
                token: None,
                port: None,
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
}
