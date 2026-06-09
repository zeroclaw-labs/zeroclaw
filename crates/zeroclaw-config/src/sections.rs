//! Curated sections surface — a flat ordered set of [`Section`]s the
//! operator walks (new install) or scans (returning user) to configure
//! a working ZeroClaw deployment.
//!
//! Every fact about a section (its enum variant, its on-the-wire key,
//! its UI shape, its help blurb, its canonical position) lives in ONE
//! table — the `sections!` invocation below. The macro expands that
//! table into the [`Section`] enum, every per-variant `match` helper,
//! and the [`QUICKSTART_SECTIONS`] const, so adding a section is exactly
//! one row, no hand-listed variant set anywhere else.
//!
//! Consumers (CLI runtime, gateway, dashboard) dispatch off this enum;
//! drift is a compile error.

use serde::{Deserialize, Serialize};

/// UI rendering shape for a section. Drives picker / form dispatch on
/// the `/config` curated section explorer and the Quickstart flow.
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

/// Humanize a section wire key for display (`risk_profiles` → `Risk profiles`,
/// `providers.models` → `Model providers`). Single source of truth for section
/// labels across the gateway dashboard, zerocode Config pane, and docs. Specific
/// wording overrides are listed explicitly; everything else is mechanically
/// title-cased from the key.
#[must_use]
pub fn humanize_section_key(key: &str) -> String {
    match key {
        "providers.models" => return "Model providers".to_string(),
        "providers.tts" => return "TTS providers".to_string(),
        "providers.transcription" => return "Transcription providers".to_string(),
        _ => {}
    }
    let mut s = key.replace(['_', '-'], " ");
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

/// Single source of truth for every pickable config section. Each row
/// maps 1:1 to a dashboard `/config/<key>` page, a CLI
/// `zeroclaw quickstart` flow and the gateway section picker handler.
/// Adding/removing a section is one row here and every consumer's
/// `match` either compiles cleanly or fails with an exhaustiveness
/// error pointing at exactly what needs an arm.
///
/// Row order is the canonical order operators see in the dashboard
/// and walk through in the CLI. It is dependency-correct: every
/// downstream alias reference an Agent carries (model_provider,
/// risk_profile, runtime_profile, channels, *_bundles) appears earlier
/// in the list than [`Section::Agents`], so walking top-to-bottom
/// never produces a dangling reference.
macro_rules! sections {
    (
        $(
            $var:ident => {
                key:   $key:literal,
                shape: $shape:ident,
                help:  $help:expr $(,)?
            }
        ),+ $(,)?
    ) => {
        /// One pickable section. The variant ordering follows the
        /// `sections!` macro invocation.
        ///
        /// With the `clap` feature on, this enum doubles as the
        /// `zeroclaw quickstart` and curated-section endpoints — no separate
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
                #[doc = $help]
                #[cfg_attr(feature = "clap", command(name = $key))]
                $var,
            )+
        }

        impl Section {
            /// Stable on-the-wire key. Also serves as the TOML
            /// top-level prefix (e.g. `providers.models.<type>.<alias>`),
            /// the curated section URL segment, and the
            /// `SectionInfo.key` field returned by the gateway.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$var => $key, )+
                }
            }

            /// Editor shape — the dashboard and the CLI both
            /// dispatch off this so the same component lights up for
            /// the same section in both surfaces.
            #[must_use]
            pub const fn shape(self) -> SectionShape {
                match self {
                    $( Self::$var => SectionShape::$shape, )+
                }
            }

            /// Per-section help blurb — single source of truth for
            /// the copy shown above the section's picker / form on
            /// every surface (CLI `ui.note(...)`, TUI heading,
            /// dashboard `SectionInfo.help`).
            #[must_use]
            pub const fn help(self) -> &'static str {
                match self {
                    $( Self::$var => $help, )+
                }
            }

            /// Human-readable section label shown in every Config surface
            /// (gateway dashboard sidebar, zerocode Config pane, docs).
            /// Single source of truth — derived from the canonical wire key
            /// so the gateway, runtime, and docs cannot disagree.
            #[must_use]
            pub fn label(self) -> String {
                humanize_section_key(self.key())
            }

            /// The canonical wire key for this section.
            #[must_use]
            pub const fn key(self) -> &'static str {
                match self {
                    $( Self::$var => $key, )+
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
                        $( $key => Some(Self::$var), )+
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

        /// Canonical ordering of sections enumerated by
        /// the Quickstart flow and the curated section explorer. The
        /// dashboard renders Next/Finish navigation against this list.
        /// Every consumer that needs section ordering reads from here.
        pub const QUICKSTART_SECTIONS: &[Section] = &[ $( Section::$var ),+ ];
    };
}

sections! {
    // Tier 1 — Brain. An agent cannot think without a model provider.
    ModelProviders => {
        key:   "providers.models",
        shape: TypedFamilyMap,
        help:  "Pick a model provider to configure (Anthropic, OpenAI, OpenRouter, \
                Ollama, custom OpenAI-compatible gateways, etc.). Multiple aliases per \
                provider are supported — e.g. anthropic.production and anthropic.dev \
                can coexist.",
    },

    // Tier 2 — Behavior shape. agents.<alias>.risk_profile and
    // .runtime_profile are required alias refs; both must exist before
    // an Agent that points at them can resolve.
    RiskProfiles => {
        key:   "risk_profiles",
        shape: OneTierAliasMap,
        help:  "Named risk profiles binding allowlists, denylists, and approval \
                thresholds. Agents reference one via `agents.<alias>.risk_profile`.",
    },
    RuntimeProfiles => {
        key:   "runtime_profiles",
        shape: OneTierAliasMap,
        help:  "Named runtime tuning profiles (token limits, retry policy, timeouts). \
                Agents reference one via `agents.<alias>.runtime_profile`.",
    },

    // Tier 3 — Storage. memory.backend points at a storage.<type>.<alias>
    // instance, so storage must exist first.
    Storage => {
        key:   "storage",
        shape: TypedFamilyMap,
        help:  "SQLite is the safe default for single-node installs (file-based, \
                zero-config, no extra services). Pick Postgres for shared or \
                multi-instance deployments, Qdrant for vector search, Markdown or \
                Lucid for human-readable files. Each backend supports multiple \
                aliased instances; agents reference them via `memory.storage_ref`.",
    },
    Memory => {
        key:   "memory",
        shape: BackendPicker,
        help:  "Persistent memory backend. SQLite is the default; pick `none` to \
                disable long-term recall entirely.",
    },

    // Tier 4 — Capabilities. Bundles that agents reference via
    // skill_bundles / mcp_bundle / knowledge_bundles.
    Skills => {
        key:   "skills",
        shape: DirectForm,
        help:  "Skills tool settings — where skill markdown lives on disk (defaults \
                to the data dir), and how the skills loader handles community \
                repositories. Add skill BUNDLES under `skill-bundles` below.",
    },
    SkillBundles => {
        key:   "skill_bundles",
        shape: OneTierAliasMap,
        help:  "Named bundles of skill files. Agents reference a bundle to load a \
                set of capabilities at startup.",
    },
    Mcp => {
        key:   "mcp",
        shape: DirectForm,
        help:  "Model Context Protocol settings. Toggle `enabled` and pick deferred \
                or eager loading. Individual MCP servers live under `mcp.servers[]`.",
    },
    McpBundles => {
        key:   "mcp_bundles",
        shape: OneTierAliasMap,
        help:  "Named bundles of MCP servers. Agents reference a bundle to pull in \
                a set of MCP tools as one unit.",
    },
    KnowledgeBundles => {
        key:   "knowledge_bundles",
        shape: OneTierAliasMap,
        help:  "Named bundles of knowledge sources (RAG indexes, doc folders). Agents \
                reference a bundle to surface relevant snippets at inference time.",
    },

    // Tier 5 — Modal IO. Optional voice in/out providers.
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

    // Tier 6 — Channels. How agents listen. agents.<alias>.channels
    // references channel aliases, so channels must exist first.
    Channels => {
        key:   "channels",
        shape: TypedFamilyMap,
        help:  "Pick which chat platforms ZeroClaw should listen on. You can \
                configure multiple — each channel gets its own alias.",
    },
    Hardware => {
        key:   "hardware",
        shape: DirectForm,
        help:  "Optional: hardware peripherals (Arduino, STM32, GPIO, etc.). \
                Skip if you don't need them.",
    },

    // Tier 7 — Bind. Pulls tiers 1–6 together. Every alias ref an
    // Agent carries exists by this point.
    // Personality is intentionally NOT a top-level section —
    // markdown personality files live per-agent and surface inside the
    // agent edit form.
    Agents => {
        key:   "agents",
        shape: OneTierAliasMap,
        help:  "An agent binds a model provider, profiles, bundles, and channels \
                into one dispatchable unit. Add one per persona; reuse the same \
                alias across channels to share state.",
    },

    // Tier 8 — Topology. Multi-agent relationships and scheduled
    // invocations; both reference agents and must follow Agents.
    PeerGroups => {
        key:   "peer_groups",
        shape: OneTierAliasMap,
        help:  "Named groups binding a channel, member agents, and external peers. \
                Mutual opt-in: two agents become peers only when both appear in the \
                same group's `agents` list.",
    },
    Cron => {
        key:   "cron",
        shape: OneTierAliasMap,
        help:  "Scheduled tasks. Each cron entry binds a schedule expression to a \
                prompt, channel, and target.",
    },

    // Tier 9 — Exposure. Gateway public-internet exposure. Only
    // relevant when a webhook-mode channel needs a public URL.
    Tunnel => {
        key:   "tunnel",
        shape: BackendPicker,
        help:  "Optional: expose your gateway over the public internet via Cloudflare \
                or ngrok. Pick `none` to keep it localhost-only.",
    },

    // Tier 10 — Lifecycle state. Not part of any agent dependency
    // chain. Tracks whether the Quickstart has completed on this
    // install; surfaces dispatch on it to decide whether to auto-open
    // the Quickstart on launch. The on-disk TOML key stays
    // `onboard_state` for backwards compatibility with installs that
    // already wrote against it; only the in-code symbol is renamed.
    QuickstartState => {
        key:   "onboard_state",
        shape: DirectForm,
        help:  "Quickstart lifecycle state. `quickstart_completed` flips to true \
                once the Quickstart finishes a successful run; while false, the \
                web gateway and TUI auto-launch the Quickstart on startup. \
                `completed_sections` is a legacy per-section ledger retained for \
                backwards compatibility with prior data.",
    },
}

impl std::fmt::Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical-order index of `section` in [`QUICKSTART_SECTIONS`].
/// Always `Some` for any valid `Section` variant — the const includes
/// every variant by construction. Returns `Option` for API symmetry
/// with [`section_index_for_key`], which can fail on unknown keys.
#[must_use]
pub fn section_index(section: Section) -> Option<usize> {
    QUICKSTART_SECTIONS.iter().position(|s| *s == section)
}

/// Canonical-order index for a wire key, or `None` if the key isn't a
/// known [`Section`]. Used by gateway / dashboard sort comparators that
/// take string keys from the HTTP layer.
#[must_use]
pub fn section_index_for_key(key: &str) -> Option<usize> {
    Section::from_key(key).and_then(section_index)
}

/// True when `key` parses as a known [`Section`].
#[must_use]
pub fn is_known_section(key: &str) -> bool {
    Section::from_key(key).is_some()
}

/// Help blurb for a section key, covering both `Section` variants and
/// the long tail of top-level `Config` fields the dashboard / TUI config
/// editor surface (gateway, scheduler, observability, …). Single source
/// of truth shared by every surface — the gateway sidebar, the CLI
/// Quickstart flow, and the future TUI config editor all call this rather
/// than maintaining parallel tables.
///
/// Resolution order:
/// 1. `Section` variants (curated `help` text next to the variant
///    declaration in the `sections!` macro).
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

/// First segment of a dotted property path mapped back to the section
/// it lives under, or `None` for non-section paths
/// (`onboard_state.completed_sections`, etc.).
#[must_use]
pub fn section_for_path(path: &str) -> Option<Section> {
    Section::from_key(path.split('.').next()?)
}

/// Does this section show any signal of having been touched on this
/// install? Used by callers (RPC config-list filtering, lifecycle
/// dispatch) to decide whether to surface a section as "untouched".
///
/// Each variant decides what counts as a real signal vs a default
/// value that round-trips identically across a fresh install.
pub fn section_has_signal(cfg: &crate::schema::Config, section: Section) -> bool {
    match section {
        Section::ModelProviders => !cfg.providers.models.is_empty(),
        // `channels.cli: bool` is a default-true scalar that lives directly
        // under `channels.*`, so a bare `starts_with("channels.")` check
        // fires on every fresh install. Require a nested channel config
        // (e.g. `channels.telegram.bot-token`) — anything with a second dot
        // segment — to count as user-driven signal.
        Section::Channels => cfg.prop_fields().iter().any(|f| {
            f.name
                .strip_prefix("channels.")
                .is_some_and(|rest| rest.contains('.'))
        }),
        Section::Hardware => cfg.hardware.enabled,
        // Memory's default backend is "sqlite" and Tunnel's is "none" —
        // both are valid user choices indistinguishable from untouched
        // defaults. TTS / transcription providers and agents start
        // empty; their existence in the typed family map IS the signal,
        // not a derivable default-divergence. Marker-only for these.
        Section::TtsProviders
        | Section::TranscriptionProviders
        | Section::Memory
        | Section::Tunnel
        | Section::Agents
        | Section::Skills
        | Section::SkillBundles
        | Section::RiskProfiles
        | Section::RuntimeProfiles
        | Section::PeerGroups
        | Section::Storage
        | Section::Cron
        | Section::Mcp
        | Section::McpBundles
        | Section::KnowledgeBundles
        | Section::QuickstartState => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip every entry in the canonical list. `from_key`,
    /// `as_str`, `section_index`, and `QUICKSTART_SECTIONS` are all
    /// generated from the same `sections!` row, so this test exercises
    /// the table — adding a row that breaks any of them fails here
    /// without listing variants by hand.
    #[test]
    fn sections_round_trip() {
        for s in QUICKSTART_SECTIONS {
            assert_eq!(Section::from_key(s.as_str()), Some(*s), "{s} round-trip");
            assert_eq!(
                section_index(*s),
                Some(QUICKSTART_SECTIONS.iter().position(|x| x == s).unwrap()),
            );
        }
        assert_eq!(Section::from_key("gateway"), None);
        assert_eq!(Section::from_key("not_a_section"), None);
    }

    /// Every section the dashboard URL surface points at must resolve
    /// through `Section::from_key`. The dashboard URL form is kebab-case
    /// (`peer-groups`), the canonical wire form may be snake_case
    /// (`peer_groups`); both must parse to the same variant.
    #[test]
    fn dashboard_url_sections_round_trip_kebab_and_snake() {
        let kebab_then_snake: &[(&str, &str, Section)] = &[
            ("peer-groups", "peer_groups", Section::PeerGroups),
            ("mcp-bundles", "mcp_bundles", Section::McpBundles),
            (
                "knowledge-bundles",
                "knowledge_bundles",
                Section::KnowledgeBundles,
            ),
            ("skill-bundles", "skill_bundles", Section::SkillBundles),
            ("risk-profiles", "risk_profiles", Section::RiskProfiles),
            (
                "runtime-profiles",
                "runtime_profiles",
                Section::RuntimeProfiles,
            ),
            ("storage", "storage", Section::Storage),
            ("cron", "cron", Section::Cron),
            ("mcp", "mcp", Section::Mcp),
        ];
        for (kebab, snake, expected) in kebab_then_snake {
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
            assert!(
                QUICKSTART_SECTIONS.contains(expected),
                "{expected:?} must be in QUICKSTART_SECTIONS",
            );
        }
    }

    /// Every OneTierAliasMap section's wire key must appear verbatim
    /// in `Config::map_key_sections()`. That table is what
    /// `Config::create_map_key` dispatches off, so a mismatch silently
    /// breaks the dashboard's `+ Add` affordance.
    #[test]
    fn alias_map_section_wire_keys_match_map_key_sections() {
        use crate::schema::Config;
        let sections = Config::map_key_sections();
        let paths: std::collections::BTreeSet<&str> = sections.iter().map(|s| s.path).collect();
        let alias_map_sections = [
            Section::PeerGroups,
            Section::Cron,
            Section::McpBundles,
            Section::KnowledgeBundles,
            Section::SkillBundles,
            Section::RiskProfiles,
            Section::RuntimeProfiles,
        ];
        for section in alias_map_sections {
            assert!(
                paths.contains(section.as_str()),
                "`Section::{section:?}.as_str() = {}` is not in map_key_sections; the \
                 picker's create_map_key call site will fail. Registered paths: {paths:?}",
                section.as_str(),
            );
        }
    }

    /// Canonical order is dependency-correct: every Section that
    /// `AliasedAgentConfig` references through an alias field appears
    /// earlier in the list than `Section::Agents`. Walking
    /// `QUICKSTART_SECTIONS` top-to-bottom never asks the operator to
    /// configure an Agent before the things it has to bind to exist.
    #[test]
    fn ordering_respects_agent_dependency_tiers() {
        let idx = |s: Section| {
            QUICKSTART_SECTIONS
                .iter()
                .position(|x| *x == s)
                .unwrap_or_else(|| panic!("{s:?} missing from QUICKSTART_SECTIONS"))
        };

        // Brain + behavior shape + bundles + channels all precede Agents.
        for upstream in [
            Section::ModelProviders,
            Section::RiskProfiles,
            Section::RuntimeProfiles,
            Section::SkillBundles,
            Section::McpBundles,
            Section::KnowledgeBundles,
            Section::Channels,
        ] {
            assert!(
                idx(upstream) < idx(Section::Agents),
                "{upstream:?} must precede Agents (Agent references it through an alias field)",
            );
        }

        // Storage precedes Memory (memory.backend = "<storage_type>.<alias>").
        assert!(
            idx(Section::Storage) < idx(Section::Memory),
            "Storage must precede Memory (memory.backend points at a storage instance)",
        );

        // Topology references agents.
        for downstream in [Section::PeerGroups, Section::Cron] {
            assert!(
                idx(Section::Agents) < idx(downstream),
                "{downstream:?} references agents and must follow Agents in the canonical order",
            );
        }
    }

    /// Storage help must steer first-time operators toward SQLite as the
    /// safe default. Pins the contract: SQLite is named, flagged as a
    /// default/safe/recommended choice, and positioned before the
    /// alternatives so the recommendation lands first instead of being
    /// buried in a closing list.
    #[test]
    fn storage_help_steers_to_sqlite_default() {
        let help = section_help("storage").to_lowercase();
        let sqlite_pos = help
            .find("sqlite")
            .expect("storage help must mention SQLite by name");
        assert!(
            help.contains("default") || help.contains("safe") || help.contains("recommend"),
            "storage help must signal SQLite is the default/safe/recommended choice; got: {help}",
        );
        for other in ["postgres", "qdrant", "markdown", "lucid"] {
            let other_pos = help.find(other).unwrap_or_else(|| {
                panic!(
                    "storage help must still name `{other}` so operators know the alternatives \
                     exist; got: {help}",
                )
            });
            assert!(
                sqlite_pos < other_pos,
                "SQLite (at {sqlite_pos}) must be mentioned before `{other}` (at {other_pos}) so \
                 the default recommendation lands first",
            );
        }
    }
}
