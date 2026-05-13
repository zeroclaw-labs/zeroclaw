//! Onboarding wizard surface — a *wizard* is the ordered set of
//! [`Section`]s the operator walks to reach a working install.
//!
//! Every fact about a section (its enum variant, its on-the-wire key,
//! its UI shape, its help blurb, its position in the wizard) lives in
//! ONE table — the [`sections!`] invocation below. The macro expands
//! that table into the [`Section`] enum, every per-variant `match`
//! helper, and the [`ONBOARDING_WIZARD`] const, so adding a section is
//! exactly one row, no hand-listed variant set anywhere else.
//!
//! Consumers (CLI runtime, gateway, dashboard) dispatch off this enum;
//! drift is a compile error.

use serde::{Deserialize, Serialize};

/// UI rendering shape for a wizard section. Drives picker / form dispatch
/// on both the `/onboard` wizard and the `/config` explorer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SectionShape {
    /// `<section>` renders a schema-driven form with no picker step.
    DirectForm,
    /// `<section>.<alias>` map of structured entries; the section page
    /// shows an alias list with `+ Add` and clicking an alias opens its
    /// schema form.
    OneTierAliasMap,
    /// `<section>.<type>.<alias>` two-tier map. Picker chooses `<type>`,
    /// alias-list step chooses `<alias>`, then the schema form opens.
    TypedFamilyMap,
    /// Single non-alias choice (memory backend, tunnel provider). Picker
    /// flips a top-level field, then the schema form for the chosen
    /// backend/provider renders.
    BackendPicker,
}

/// Single source of truth for every pickable config section. Each row
/// maps 1:1 to a dashboard `/config/<key>` page, a CLI
/// `zeroclaw onboard <key>` subcommand, and the gateway picker handler.
/// Adding/removing a section is one row here and every consumer's
/// `match` either compiles cleanly or fails with an exhaustiveness
/// error pointing at exactly what needs an arm.
///
/// Rows are split into two groups:
///
/// * `wizard_steps` — the ordered initial-setup flow walked by `/onboard`.
///   Order in this group is the canonical wizard order — structural
///   sections first, agents last. [`ONBOARDING_WIZARD`] is
///   generated from this group only.
/// * `explorer_only` — map-keyed sections that operators discover via
///   `/config/<key>` or `zeroclaw onboard <key>` after the wizard
///   completes. Not part of the initial wizard order; never auto-prompted.
///
/// Both groups feed the [`Section`] enum, its clap subcommand surface,
/// and every per-variant `match` helper — so any consumer that already
/// matched on a wizard variant will fail to compile until it adds arms
/// for the explorer variants too.
macro_rules! sections {
    (
        wizard_steps: {
            $(
                $wvar:ident => {
                    key:   $wkey:literal,
                    shape: $wshape:ident,
                    help:  $whelp:expr $(,)?
                }
            ),+ $(,)?
        },
        explorer_only: {
            $(
                $evar:ident => {
                    key:   $ekey:literal,
                    shape: $eshape:ident,
                    help:  $ehelp:expr $(,)?
                }
            ),+ $(,)?
        } $(,)?
    ) => {
        /// One pickable section. The variant ordering follows the
        /// `sections!` macro invocation: wizard_steps first (canonical
        /// wizard order), then explorer_only.
        ///
        /// With the `clap` feature on, this enum doubles as the
        /// `zeroclaw onboard <section>` clap subcommand — no separate
        /// mirror enum in the binary crate.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[cfg_attr(feature = "clap", derive(clap::Subcommand))]
        #[serde(rename_all = "snake_case")]
        pub enum Section {
            $(
                // Both clap (`--help`) and our runtime `help()` method
                // need the same blurb; emit it once as a doc comment so
                // the two surfaces share a single string per variant.
                #[doc = $whelp]
                #[cfg_attr(feature = "clap", command(name = $wkey))]
                $wvar,
            )+
            $(
                #[doc = $ehelp]
                #[cfg_attr(feature = "clap", command(name = $ekey))]
                $evar,
            )+
        }

        impl Section {
            /// Stable on-the-wire key — appears as the TOML top-level
            /// prefix (`model_providers.<type>.<alias>`), the
            /// `/onboard/<key>` URL segment, and the `SectionInfo.key`
            /// field returned by the gateway.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$wvar => $wkey, )+
                    $( Self::$evar => $ekey, )+
                }
            }

            /// Editor shape — the dashboard and the wizard both
            /// dispatch off this so the same component lights up for
            /// the same section in both surfaces.
            #[must_use]
            pub const fn shape(self) -> SectionShape {
                match self {
                    $( Self::$wvar => SectionShape::$wshape, )+
                    $( Self::$evar => SectionShape::$eshape, )+
                }
            }

            /// Per-section help blurb — single source of truth for
            /// the copy shown above the section's picker / form on
            /// every surface (CLI `ui.note(...)`, TUI heading,
            /// dashboard `SectionInfo.help`).
            #[must_use]
            pub const fn help(self) -> &'static str {
                match self {
                    $( Self::$wvar => $whelp, )+
                    $( Self::$evar => $ehelp, )+
                }
            }

            /// True when this section is part of the initial-setup
            /// wizard (`/onboard` and its CLI counterpart). False for
            /// explorer-only sections that operators reach via
            /// `/config/<key>` after the wizard completes.
            #[must_use]
            pub const fn is_wizard_step(self) -> bool {
                match self {
                    $( Self::$wvar => true, )+
                    $( Self::$evar => false, )+
                }
            }

            /// Parse a stable wire key, tolerating both the snake and
            /// kebab spellings of any section. The schema mixes the two:
            /// `model_providers` (snake) and `peer-groups` (kebab) are
            /// both valid wire forms produced elsewhere in the codebase.
            /// Callers (dashboard URL routing, gateway picker dispatch,
            /// CLI clap subcommands) can pass either form; `from_key`
            /// resolves to the same variant. Returns `None` for keys
            /// outside the known section table. Named `from_key` rather
            /// than `from_str` so clippy doesn't flag it as confusable
            /// with `std::str::FromStr` (parse failure is `None`, not
            /// `Err(_)`).
            #[must_use]
            pub fn from_key(s: &str) -> Option<Self> {
                let try_match = |s: &str| -> Option<Self> {
                    match s {
                        $( $wkey => Some(Self::$wvar), )+
                        $( $ekey => Some(Self::$evar), )+
                        _ => None,
                    }
                };
                if let Some(v) = try_match(s) {
                    return Some(v);
                }
                if s.contains('_')
                    && let Some(v) = try_match(&s.replace('_', "-"))
                {
                    return Some(v);
                }
                if s.contains('-')
                    && let Some(v) = try_match(&s.replace('-', "_"))
                {
                    return Some(v);
                }
                None
            }
        }

        /// The onboarding wizard: an ordered slice of [`Section`]s
        /// walked during `/onboard`. Generated from the `wizard_steps`
        /// group only — explorer_only variants are deliberately
        /// excluded so the initial-setup flow stays focused on
        /// must-configure-first sections.
        pub const ONBOARDING_WIZARD: &[Section] = &[ $( Section::$wvar ),+ ];
    };
}

sections! {
    wizard_steps: {
        ModelProviders => {
            key:   "providers.models",
            shape: TypedFamilyMap,
            help:  "Pick a model provider to configure (Anthropic, OpenAI, OpenRouter, \
                    Ollama, custom OpenAI-compatible gateways, etc.). Multiple aliases per \
                    provider are supported — e.g. anthropic.production and anthropic.dev \
                    can coexist.",
        },
        TtsProviders => {
            key:   "providers.tts",
            shape: TypedFamilyMap,
            help:  "Text-to-speech providers (OpenAI, ElevenLabs, Google, Edge, Piper). \
                    Configure one per voice / language; agents reference them by alias.",
        },
        TranscriptionProviders => {
            key:   "providers.transcription",
            shape: TypedFamilyMap,
            help:  "Speech-to-text providers (OpenAI Whisper, Groq, Deepgram, AssemblyAI, \
                    Google, local Whisper). Configure one per pipeline; agents reference \
                    them by alias.",
        },
        Channels => {
            key:   "channels",
            shape: TypedFamilyMap,
            help:  "Pick which chat platforms ZeroClaw should listen on. You can \
                    configure multiple — each channel gets its own alias.",
        },
        Memory => {
            key:   "memory",
            shape: BackendPicker,
            help:  "Persistent memory backend. SQLite is the default; pick `none` to \
                    disable long-term recall entirely.",
        },
        Hardware => {
            key:   "hardware",
            shape: DirectForm,
            help:  "Optional: hardware peripherals (Arduino, STM32, GPIO, etc.). \
                    Skip if you don't need them.",
        },
        Tunnel => {
            key:   "tunnel",
            shape: BackendPicker,
            help:  "Optional: expose your gateway over the public internet via Cloudflare \
                    or ngrok. Pick `none` to keep it localhost-only.",
        },
        // Personality is intentionally NOT a wizard section in v0.8.0 —
        // markdown personality files live per-agent and surface inside the
        // agent edit form.
        Agents => {
            key:   "agents",
            shape: OneTierAliasMap,
            help:  "An agent binds a model provider, profiles, bundles, and channels \
                    into one dispatchable unit. Add one per persona; reuse the same \
                    alias across channels to share state.",
        },
        Skills => {
            key:   "skills",
            shape: DirectForm,
            help:  "Skills tool settings — where skill markdown lives on disk (defaults \
                    to the data dir), and how the skills loader handles community \
                    repositories. Add skill BUNDLES under `skill-bundles` below.",
        },
        SkillBundles => {
            key:   "skill-bundles",
            shape: OneTierAliasMap,
            help:  "Named bundles of skill files. Agents reference a bundle to load a \
                    set of capabilities at startup.",
        },
        RiskProfiles => {
            key:   "risk-profiles",
            shape: OneTierAliasMap,
            help:  "Named risk profiles binding allowlists, denylists, and approval \
                    thresholds. Agents reference one via `agents.<alias>.risk_profile`.",
        },
        RuntimeProfiles => {
            key:   "runtime-profiles",
            shape: OneTierAliasMap,
            help:  "Named runtime tuning profiles (token limits, retry policy, timeouts). \
                    Agents reference one via `agents.<alias>.runtime_profile`.",
        },
        PeerGroups => {
            key:   "peer-groups",
            shape: OneTierAliasMap,
            help:  "Named groups binding a channel, member agents, and external peers. \
                    Mutual opt-in: two agents become peers only when both appear in the \
                    same group's `agents` list.",
        },
    },
    explorer_only: {
        // Wire keys MUST match the schema's `Config::map_key_sections()`
        // output verbatim. The Configurable derive runs the field name
        // through `snake_to_kebab`, so any field with an underscore in
        // its Rust name (peer_groups, risk_profiles, ...) registers as
        // kebab in the map-key section table. `Section::as_str()` is
        // used as the section_path argument to `create_map_key`, so it
        // must match the registered form. `from_key` normalizes either
        // form on input so dashboard URLs and CLI invocations work for
        // both spellings.
        Storage => {
            key:   "storage",
            shape: TypedFamilyMap,
            help:  "Storage backend instances (sqlite, postgres, qdrant, markdown, lucid). \
                    Each backend can have multiple aliased instances; agents reference \
                    them via `memory.storage_ref`.",
        },
        Cron => {
            key:   "cron",
            shape: OneTierAliasMap,
            help:  "Scheduled tasks. Each cron entry binds a schedule expression to a \
                    prompt, channel, and target.",
        },
        Mcp => {
            key:   "mcp",
            shape: DirectForm,
            help:  "Model Context Protocol settings. Toggle `enabled` and pick deferred \
                    or eager loading. Individual MCP servers live under `mcp.servers[]`.",
        },
        McpBundles => {
            key:   "mcp-bundles",
            shape: OneTierAliasMap,
            help:  "Named bundles of MCP servers. Agents reference a bundle to pull in \
                    a set of MCP tools as one unit.",
        },
        KnowledgeBundles => {
            key:   "knowledge-bundles",
            shape: OneTierAliasMap,
            help:  "Named bundles of knowledge sources (RAG indexes, doc folders). Agents \
                    reference a bundle to surface relevant snippets at inference time.",
        },
    },
}

impl std::fmt::Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Index of `section` in [`ONBOARDING_WIZARD`], or `None` for
/// explorer-only sections that are intentionally excluded from the
/// initial-setup flow. Pre-split this returned `usize` and panicked
/// on a missing variant; with the explorer_only group the panic became
/// a real boot crash on any Section the dashboard sort comparator
/// looked up that wasn't a wizard step.
#[must_use]
pub fn wizard_index(section: Section) -> Option<usize> {
    ONBOARDING_WIZARD.iter().position(|s| *s == section)
}

/// Canonical-order index for a wire key, or `None` if the key isn't a
/// wizard section (either it's explorer-only, or it isn't a Section at
/// all). Used by gateway / dashboard sort comparators that take string
/// keys from the HTTP layer.
#[must_use]
pub fn wizard_index_for_key(key: &str) -> Option<usize> {
    Section::from_key(key).and_then(wizard_index)
}

/// True when `key` parses as a wizard section.
#[must_use]
pub fn is_wizard_section(key: &str) -> bool {
    Section::from_key(key).is_some()
}

/// Help blurb for a section key, covering both `Section` variants and
/// the long tail of top-level `Config` fields the dashboard / TUI config
/// editor surface (gateway, scheduler, observability, …). Single source
/// of truth shared by every surface — the gateway sidebar, the CLI
/// wizard, and the future TUI config editor all call this rather than
/// maintaining parallel tables.
///
/// Resolution order:
/// 1. Wizard / explorer-only `Section` variants (curated `help` text
///    next to the variant declaration in the `sections!` macro).
/// 2. The `Config` struct's `#[nested]` field-level `///` docstring,
///    harvested by the `Configurable` derive into
///    `Config::nested_section_help`. This is what makes adding a new
///    top-level section a one-line schema change with no parallel
///    help table to update.
///
/// Returns `""` for keys without a docstring so callers can decide
/// whether to omit the help row or show a fallback.
#[must_use]
pub fn section_help(key: &str) -> &'static str {
    if let Some(s) = Section::from_key(key) {
        return s.help();
    }
    crate::schema::Config::nested_section_help(key).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every entry in the canonical wizard. `from_key`,
    /// `as_str`, and `ONBOARDING_WIZARD` are all generated from the
    /// same `sections!` row, so this test exercises the table — adding
    /// a row that breaks any of them fails here without listing
    /// variants by hand.
    #[test]
    fn wizard_round_trips() {
        for s in ONBOARDING_WIZARD {
            assert_eq!(Section::from_key(s.as_str()), Some(*s), "{s} round-trip");
            assert_eq!(
                wizard_index(*s),
                Some(ONBOARDING_WIZARD.iter().position(|x| x == s).unwrap()),
            );
        }
        assert_eq!(Section::from_key("gateway"), None);
        assert_eq!(Section::from_key("not_a_section"), None);
    }

    /// Explorer-only variants are deliberately absent from
    /// `ONBOARDING_WIZARD` — `wizard_index` returns `None` for them
    /// (used to panic, which crashed the gateway boot path).
    #[test]
    fn wizard_index_returns_none_for_explorer_only_sections() {
        let explorer = [
            Section::Storage,
            Section::Cron,
            Section::Mcp,
            Section::McpBundles,
            Section::KnowledgeBundles,
        ];
        for s in explorer {
            assert_eq!(
                wizard_index(s),
                None,
                "{s:?} is explorer-only and must not have a wizard index",
            );
            assert_eq!(
                wizard_index_for_key(s.as_str()),
                None,
                "wizard_index_for_key({}) must be None for explorer-only sections",
                s.as_str(),
            );
        }
    }

    /// Every section the dashboard URL surface points at must resolve
    /// through `Section::from_key`. The dashboard URL form is kebab-case
    /// (`peer-groups`), the canonical wire form is snake_case
    /// (`peer_groups`); both must parse to the same variant.
    #[test]
    fn dashboard_url_sections_round_trip_kebab_and_snake() {
        let kebab_then_snake: &[(&str, &str, Section, bool)] = &[
            ("peer-groups", "peer_groups", Section::PeerGroups, true),
            ("mcp-bundles", "mcp_bundles", Section::McpBundles, false),
            (
                "knowledge-bundles",
                "knowledge_bundles",
                Section::KnowledgeBundles,
                false,
            ),
            (
                "skill-bundles",
                "skill_bundles",
                Section::SkillBundles,
                true,
            ),
            (
                "risk-profiles",
                "risk_profiles",
                Section::RiskProfiles,
                true,
            ),
            (
                "runtime-profiles",
                "runtime_profiles",
                Section::RuntimeProfiles,
                true,
            ),
            ("storage", "storage", Section::Storage, false),
            ("cron", "cron", Section::Cron, false),
            ("mcp", "mcp", Section::Mcp, false),
        ];
        for (kebab, snake, expected, is_wizard) in kebab_then_snake {
            assert_eq!(
                Section::from_key(kebab),
                Some(*expected),
                "kebab `{kebab}` should resolve to {expected:?}",
            );
            assert_eq!(
                Section::from_key(snake),
                Some(*expected),
                "snake `{snake}` should resolve to {expected:?}",
            );
            assert_eq!(
                expected.is_wizard_step(),
                *is_wizard,
                "{expected:?} wizard_step contract mismatch",
            );
            assert_eq!(
                ONBOARDING_WIZARD.contains(expected),
                *is_wizard,
                "{expected:?} ONBOARDING_WIZARD membership mismatch",
            );
        }
    }

    /// Wizard sections keep their `is_wizard_step() == true` contract.
    /// Together with the explorer-only assertion above, this pins the
    /// macro's wizard / explorer split.
    #[test]
    fn wizard_sections_are_marked_as_wizard_steps() {
        for s in ONBOARDING_WIZARD {
            assert!(
                s.is_wizard_step(),
                "{s:?} is in ONBOARDING_WIZARD but is_wizard_step() returned false",
            );
        }
    }

    /// Every explorer_only OneTierAliasMap section's wire key must
    /// appear verbatim in `Config::map_key_sections()`. That table is
    /// what `Config::create_map_key` dispatches off, so a mismatch
    /// silently breaks the dashboard's `+ Add` affordance. Asserts that
    /// `Section::as_str()` for each explorer variant matches the
    /// schema's registered path.
    #[test]
    fn explorer_only_section_wire_keys_match_map_key_sections() {
        use crate::schema::Config;
        let sections = Config::map_key_sections();
        let paths: std::collections::BTreeSet<&str> = sections.iter().map(|s| s.path).collect();
        let explorer = [
            Section::PeerGroups,
            Section::Cron,
            Section::McpBundles,
            Section::KnowledgeBundles,
            Section::SkillBundles,
            Section::RiskProfiles,
            Section::RuntimeProfiles,
        ];
        for section in explorer {
            assert!(
                paths.contains(section.as_str()),
                "`Section::{section:?}.as_str() = {}` is not in map_key_sections; the \
                 picker's create_map_key call site will fail. Registered paths: {paths:?}",
                section.as_str(),
            );
        }
    }
}
