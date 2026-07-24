//! Curated sections surface — a flat ordered set of [`Section`]s the
//! operator walks (new install) or scans (returning user) to configure
//! a working ZeroClaw deployment.

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

/// Display group for a curated or schema-derived configuration section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SectionGroup {
    /// Quickstart-walked essentials: providers, profiles, channels,
    /// agents — the most-edited sections, surfaced first.
    Foundation,
    /// Agent loop, scheduling, and orchestration tuning.
    Agent,
    /// Multi-agent / delegation.
    MultiAgent,
    /// Tool integrations the agent can call.
    Tools,
    /// External services / vendor integrations.
    Integrations,
    /// Networking / multi-node infrastructure.
    Network,
    /// Storage, identity, secrets.
    Storage,
    /// Operations / monitoring / safety / cost.
    Operations,
    /// Catch-all for keys no one has curated yet.
    Other,
}

impl SectionGroup {
    /// UI label. These exact strings are what `ConfigSectionEntry.group`
    /// carries on the wire and what the dashboard's `GROUP_ORDER`
    /// (web/src/pages/Config.tsx) and the zerocode Config pane group
    /// by — change one, change all of them together.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Foundation => "Foundation",
            Self::Agent => "Agent",
            Self::MultiAgent => "Multi-agent",
            Self::Tools => "Tools",
            Self::Integrations => "Integrations",
            Self::Network => "Network",
            Self::Storage => "Storage",
            Self::Operations => "Operations",
            Self::Other => "Other",
        }
    }

    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        SECTION_GROUPS.iter().copied().find(|g| g.label() == label)
    }
}

impl std::fmt::Display for SectionGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Canonical display order for section groups across every Config
/// surface (dashboard sidebar, zerocode Config pane). "All sections"
/// is not a group — a surface that offers a flat default view renders
/// that view itself; this list orders the grouped presentation.
pub const SECTION_GROUPS: &[SectionGroup] = &[
    SectionGroup::Foundation,
    SectionGroup::Agent,
    SectionGroup::MultiAgent,
    SectionGroup::Tools,
    SectionGroup::Integrations,
    SectionGroup::Network,
    SectionGroup::Storage,
    SectionGroup::Operations,
    SectionGroup::Other,
];

#[must_use]
pub fn section_group_for_key(key: &str) -> SectionGroup {
    if let Some(s) = Section::from_key(key) {
        return s.group();
    }
    crate::schema::Config::nested_section_group(key)
        .or_else(|| crate::schema::Config::nested_section_group(&key.replace('-', "_")))
        .and_then(SectionGroup::from_label)
        .unwrap_or(SectionGroup::Other)
}

#[must_use]
pub fn humanize_section_key(key: &str) -> String {
    match key {
        "providers.models" => return "Model providers".to_string(),
        "providers.tts" => return "TTS providers".to_string(),
        "providers.transcription" => return "Transcription providers".to_string(),
        _ => {}
    }
    let mut s = key.replace(['_', '-', '.'], " ");
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

macro_rules! sections {
    (
        $(
            $var:ident => {
                key:   $key:literal,
                shape: $shape:ident,
                group: $group:ident,
                help:  $help:expr $(,)?
            }
        ),+ $(,)?
    ) => {
        /// One configuration section exposed by the curated section registry.
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

            #[must_use]
            pub const fn group(self) -> SectionGroup {
                match self {
                    $( Self::$var => SectionGroup::$group, )+
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
        group: Foundation,
        help:  "Pick a model provider to configure (Anthropic, OpenAI, OpenRouter, \
                Ollama, custom OpenAI-compatible gateways, etc.). Multiple aliases per \
                provider are supported — e.g. anthropic.production and anthropic.dev \
                can coexist.",
    },

    // Tier 1b — Routing. Named hints that map a short alias to a
    // provider + model combo. Routes depend on providers, so they
    // follow Tier 1.
    ModelRoutes => {
        key:   "model_routes",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "Named model routing hints (e.g. reasoning, fast, code). Each \
                route maps a hint to a specific provider + model combo. Use \
                `hint:<name>` as the model parameter to dispatch through a route.",
    },
    EmbeddingRoutes => {
        key:   "embedding_routes",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "Named embedding routing hints (e.g. semantic, archive, faq). \
                Each route maps a hint to an embedding-capable provider + model \
                combo. Use `hint:<name>` as the embedding_model parameter.",
    },

    // Tier 2 — Behavior shape. agents.<alias>.risk_profile and
    // .runtime_profile are required alias refs; both must exist before
    // an Agent that points at them can resolve.
    RiskProfiles => {
        key:   "risk_profiles",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "Named risk profiles binding allowlists, denylists, and approval \
                thresholds. Agents reference one via `agents.<alias>.risk_profile`.",
    },
    RuntimeProfiles => {
        key:   "runtime_profiles",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "Named runtime tuning profiles (token limits, retry policy, timeouts). \
                Agents reference one via `agents.<alias>.runtime_profile`.",
    },

    // Tier 3 — Storage. memory.backend points at a storage.<type>.<alias>
    // instance, so storage must exist first.
    Storage => {
        key:   "storage",
        shape: TypedFamilyMap,
        group: Storage,
        help:  "SQLite is the safe default for single-node installs (file-based, \
                zero-config, no extra services). Pick Postgres for shared or \
                multi-instance deployments, Qdrant for vector search, Markdown or \
                Lucid for human-readable files. Each backend supports multiple \
                aliased instances; agents reference them via `memory.storage_ref`.",
    },
    Memory => {
        key:   "memory",
        shape: BackendPicker,
        group: Foundation,
        help:  "Persistent memory backend. SQLite is the default; pick `none` to \
                disable long-term recall entirely.",
    },

    // Tier 4 — Capabilities. Bundles that agents reference via
    // skill_bundles / mcp_bundle / knowledge_bundles.
    Skills => {
        key:   "skills",
        shape: DirectForm,
        group: Foundation,
        help:  "Skills tool settings — where skill markdown lives on disk (defaults \
                to the data dir), and how the skills loader handles community \
                repositories. Add skill BUNDLES under `skill-bundles` below.",
    },
    SkillBundles => {
        key:   "skill_bundles",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "Named bundles of skill files. Agents reference a bundle to load a \
                set of capabilities at startup.",
    },
    Mcp => {
        key:   "mcp",
        shape: DirectForm,
        group: Tools,
        help:  "Model Context Protocol settings. Toggle `enabled` and pick deferred \
                or eager loading. Individual MCP servers live under `mcp.servers[]`.",
    },
    McpServers => {
        key:   "mcp.servers",
        shape: OneTierAliasMap,
        group: Tools,
        help:  "Individual Model Context Protocol servers. Each entry binds a \
                transport (stdio, http, sse), the command or URL to reach it, \
                optional headers, and a `tool_timeout_secs` cap (≤ 600). Each \
                server's `name` is its addressable key — rename via the section \
                page rather than editing the field directly. Group servers \
                into bundles under `mcp_bundles` below.",
    },
    McpBundles => {
        key:   "mcp_bundles",
        shape: OneTierAliasMap,
        group: Tools,
        help:  "Named bundles of MCP servers, granted to agents that list the bundle \
                in their `mcp_bundles`. Secure by default: an agent gets only the \
                servers its bundles grant; with no bundle it gets no MCP servers.",
    },
    KnowledgeBundles => {
        key:   "knowledge_bundles",
        shape: OneTierAliasMap,
        group: Tools,
        help:  "Named bundles of knowledge sources (RAG indexes, doc folders). Agents \
                reference a bundle to surface relevant snippets at inference time.",
    },

    // Tier 5 — Modal IO. Optional voice in/out providers.
    TtsProviders => {
        key:   "providers.tts",
        shape: TypedFamilyMap,
        group: Foundation,
        help:  "Text-to-speech providers (OpenAI, ElevenLabs, Google, Edge, Piper). \
                Configure one per voice / language; agents reference them by alias.",
    },
    TranscriptionProviders => {
        key:   "providers.transcription",
        shape: TypedFamilyMap,
        group: Foundation,
        help:  "Speech-to-text providers (OpenAI Whisper, Groq, Deepgram, AssemblyAI, \
                Google, local Whisper). Configure one per pipeline; agents reference \
                them by alias.",
    },

    // Tier 6 — Channels. How agents listen. agents.<alias>.channels
    // references channel aliases, so channels must exist first.
    Channels => {
        key:   "channels",
        shape: TypedFamilyMap,
        group: Foundation,
        help:  "Pick which chat platforms ZeroClaw should listen on. Global \
                channel settings live on `[channels]`; each configured platform \
                still gets its own alias.",
    },
    Hardware => {
        key:   "hardware",
        shape: DirectForm,
        group: Foundation,
        help:  "Optional: hardware peripherals (Arduino, STM32, GPIO, etc.). \
                Skip if you don't need them.",
    },

    Agents => {
        key:   "agents",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "An agent binds a model provider, profiles, bundles, and channels \
                into one dispatchable unit. Add one per persona; reuse the same \
                alias across channels to share state.",
    },

    // Tier 8 — Topology. Multi-agent relationships and scheduled
    // invocations; both reference agents and must follow Agents.
    PeerGroups => {
        key:   "peer_groups",
        shape: OneTierAliasMap,
        group: Foundation,
        help:  "Named groups binding a channel, member agents, and external peers. \
                Mutual opt-in: two agents become peers only when both appear in the \
                same group's `agents` list.",
    },
    Cron => {
        key:   "cron",
        shape: OneTierAliasMap,
        group: Agent,
        help:  "Scheduled tasks. Each cron entry binds a schedule expression to a \
                prompt, channel, and target.",
    },

    // Tier 9 — Exposure. Gateway public-internet exposure. Only
    // relevant when a webhook-mode channel needs a public URL.
    Tunnel => {
        key:   "tunnel",
        shape: BackendPicker,
        group: Foundation,
        help:  "Optional: expose your gateway over the public internet via Cloudflare \
                or ngrok. Pick `none` to keep it localhost-only.",
    },

    QuickstartState => {
        key:   "onboard_state",
        shape: DirectForm,
        group: Operations,
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

pub fn section_has_signal(cfg: &crate::schema::Config, section: Section) -> bool {
    match section {
        Section::ModelProviders => !cfg.providers.models.is_empty(),
        Section::Channels => cfg.prop_fields().iter().any(|f| {
            f.name
                .strip_prefix("channels.")
                .is_some_and(|rest| rest.contains('.'))
        }),
        Section::Hardware => cfg.hardware.enabled,
        Section::McpServers => !cfg.mcp.servers.is_empty(),
        // Routes' existence in the Vec is the signal, same as McpServers.
        Section::ModelRoutes => !cfg.model_routes.is_empty(),
        Section::EmbeddingRoutes => !cfg.embedding_routes.is_empty(),
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

    #[test]
    fn humanize_strips_dots_underscores_and_hyphens() {
        assert_eq!(humanize_section_key("mcp.servers"), "Mcp servers");
        assert_eq!(humanize_section_key("mcp_bundles"), "Mcp bundles");
        assert_eq!(
            humanize_section_key("knowledge-bundles"),
            "Knowledge bundles"
        );
        assert_eq!(Section::McpServers.label(), "Mcp servers");
        assert_eq!(Section::McpBundles.label(), "Mcp bundles");
    }

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

    #[test]
    fn alias_map_section_wire_keys_match_map_key_sections() {
        use crate::schema::Config;
        let sections = Config::map_key_sections();
        let paths: std::collections::BTreeSet<&str> = sections.iter().map(|s| s.path).collect();
        let alias_map_sections = [
            Section::PeerGroups,
            Section::Cron,
            Section::McpServers,
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

    #[test]
    fn mcp_servers_section_has_alias_map_shape_and_parent_keeps_direct_form() {
        assert_eq!(Section::Mcp.shape(), SectionShape::DirectForm);
        assert_eq!(Section::McpServers.shape(), SectionShape::OneTierAliasMap);
        assert!(QUICKSTART_SECTIONS.contains(&Section::Mcp));
        assert!(QUICKSTART_SECTIONS.contains(&Section::McpServers));
        assert!(QUICKSTART_SECTIONS.contains(&Section::McpBundles));

        // Canonical order: parent settings come first, then the
        // servers editor, then the bundles map. Operators walking the
        // Quickstart hit the toggle before the per-server form.
        let idx = |s: Section| {
            QUICKSTART_SECTIONS
                .iter()
                .position(|x| *x == s)
                .unwrap_or_else(|| panic!("{s:?} missing from QUICKSTART_SECTIONS"))
        };
        assert!(idx(Section::Mcp) < idx(Section::McpServers));
        assert!(idx(Section::McpServers) < idx(Section::McpBundles));
    }

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

    #[test]
    fn section_groups_const_is_exhaustive_unique_and_other_last() {
        // Exhaustiveness guard: adding a SectionGroup variant without
        // updating this list (and SECTION_GROUPS) fails to compile here.
        let all = [
            SectionGroup::Foundation,
            SectionGroup::Agent,
            SectionGroup::MultiAgent,
            SectionGroup::Tools,
            SectionGroup::Integrations,
            SectionGroup::Network,
            SectionGroup::Storage,
            SectionGroup::Operations,
            SectionGroup::Other,
        ];
        for g in all {
            match g {
                SectionGroup::Foundation
                | SectionGroup::Agent
                | SectionGroup::MultiAgent
                | SectionGroup::Tools
                | SectionGroup::Integrations
                | SectionGroup::Network
                | SectionGroup::Storage
                | SectionGroup::Operations
                | SectionGroup::Other => {}
            }
            assert!(
                SECTION_GROUPS.contains(&g),
                "{g:?} missing from SECTION_GROUPS",
            );
        }
        assert_eq!(
            SECTION_GROUPS.len(),
            all.len(),
            "duplicate group in SECTION_GROUPS"
        );
        assert_eq!(
            SECTION_GROUPS.last(),
            Some(&SectionGroup::Other),
            "Other must render last in every grouped surface",
        );
    }

    #[test]
    fn group_labels_are_pinned_for_ui_compat() {
        let expected = [
            "Foundation",
            "Agent",
            "Multi-agent",
            "Tools",
            "Integrations",
            "Network",
            "Storage",
            "Operations",
            "Other",
        ];
        for (g, want) in SECTION_GROUPS.iter().zip(expected) {
            assert_eq!(g.label(), want);
            assert_eq!(g.to_string(), want);
        }
    }

    #[test]
    fn curated_sections_never_fall_into_other() {
        for s in QUICKSTART_SECTIONS {
            assert_ne!(
                s.group(),
                SectionGroup::Other,
                "Section::{s:?} needs a real group in its sections! row",
            );
        }
    }

    #[test]
    fn section_group_for_key_resolves_curated_tail_and_unknown() {
        // Curated rows, including former gap sections.
        assert_eq!(
            section_group_for_key("providers.models"),
            SectionGroup::Foundation
        );
        assert_eq!(
            section_group_for_key("providers.tts"),
            SectionGroup::Foundation
        );
        assert_eq!(section_group_for_key("mcp.servers"), SectionGroup::Tools);
        assert_eq!(section_group_for_key("mcp_bundles"), SectionGroup::Tools);
        assert_eq!(
            section_group_for_key("knowledge_bundles"),
            SectionGroup::Tools
        );
        assert_eq!(section_group_for_key("cron"), SectionGroup::Agent);
        assert_eq!(section_group_for_key("storage"), SectionGroup::Storage);
        // Kebab spelling resolves like snake.
        assert_eq!(
            section_group_for_key("peer-groups"),
            SectionGroup::Foundation
        );
        // Hand-mapped long tail (formerly in the gateway).
        assert_eq!(section_group_for_key("gateway"), SectionGroup::Network);
        assert_eq!(
            section_group_for_key("observability"),
            SectionGroup::Operations
        );
        assert_eq!(section_group_for_key("delegate"), SectionGroup::MultiAgent);
        assert_eq!(section_group_for_key("web_search"), SectionGroup::Tools);
        assert_eq!(section_group_for_key("secrets"), SectionGroup::Storage);
        // Kebab spelling of a SCHEMA-grouped (non-curated) root resolves
        // through the kebab→snake fallback to the snake-keyed
        // `nested_section_group` arm. (`web_search`/`data_retention` are
        // schema `#[group]` fields, not curated `sections!` rows.)
        assert_eq!(section_group_for_key("web-search"), SectionGroup::Tools);
        assert_eq!(
            section_group_for_key("data-retention"),
            SectionGroup::Operations
        );
        // Unknown keys land in the catch-all, never disappear.
        assert_eq!(section_group_for_key("not_a_section"), SectionGroup::Other);
    }

    #[test]
    fn every_surfaced_root_has_a_group() {
        use crate::schema::Config;
        let cfg = Config::default();
        let roots: std::collections::BTreeSet<String> = cfg
            .prop_fields()
            .iter()
            .filter_map(|f| f.name.split('.').next().map(str::to_string))
            .collect();

        // System/bookkeeping root the explorer hides; resolved at
        // runtime, never user-edited, so intentionally ungrouped.
        const HIDDEN: &[&str] = &["schema_version"];

        const UNGROUPED: &[&str] = &[
            "escalation",
            "locale",
            "microsoft365",
            "file_upload",
            "file_upload_bundle",
            "file_download",
            "wss",
        ];

        let violations: Vec<&String> = roots
            .iter()
            .filter(|r| !HIDDEN.contains(&r.as_str()) && !UNGROUPED.contains(&r.as_str()))
            .filter(|r| section_group_for_key(r) == SectionGroup::Other)
            .collect();
        assert!(
            violations.is_empty(),
            "these surfaced config roots resolve to SectionGroup::Other — add a \
             `#[group = \"...\"]` to each in schema.rs (or, if intentionally \
             uncurated, to the UNGROUPED allowlist): {violations:?}",
        );

        // Keep the allowlist from rotting: each entry must still be a
        // surfaced root and still ungrouped.
        for u in UNGROUPED {
            assert!(
                roots.contains(*u),
                "UNGROUPED lists `{u}` but it is no longer a surfaced root — remove it",
            );
            assert_eq!(
                section_group_for_key(u),
                SectionGroup::Other,
                "`{u}` now resolves to a real group — remove it from UNGROUPED",
            );
        }
    }

    #[test]
    fn migrated_hand_list_roots_keep_their_groups() {
        let expected = [
            ("claude_code", SectionGroup::Integrations),
            ("codex_cli", SectionGroup::Integrations),
            ("gemini_cli", SectionGroup::Integrations),
            ("opencode_cli", SectionGroup::Integrations),
            ("sop", SectionGroup::Agent),
            ("verifiable_intent", SectionGroup::Agent),
            ("shell_tool", SectionGroup::Tools),
            ("observability", SectionGroup::Operations),
            ("gateway", SectionGroup::Network),
            ("delegate", SectionGroup::MultiAgent),
            ("secrets", SectionGroup::Storage),
        ];
        for (key, group) in expected {
            assert_eq!(
                section_group_for_key(key),
                group,
                "`{key}` should resolve to {group:?} via its #[group] attribute",
            );
        }
    }

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
