//! Resolved authorization grants — the single runtime authorization
//! vocabulary (RFC 7141).
//!
//! The grant vocabulary is resource × verb over enum-typed resource classes,
//! plus three fine-grain selectors (agent aliases, config write paths, tool
//! names) and an `admin` short-circuit. Providers never construct these:
//! the shared principal resolver maps verified claims / roster entries
//! through configured permission profiles into one merged [`ResolvedGrants`],
//! re-resolved whenever the authorization-policy generation changes.
//!
//! Deny-by-default, Rev 8 semantics: an empty grant set permits nothing,
//! and an EMPTY SELECTOR LIST GRANTS NO INSTANCES — broad access requires
//! the explicit [`WILDCARD`] selector or an administrator grant. Every RPC
//! method and gateway route classifies itself to exactly one
//! `(Resource, Verb)` pair, so a request either matches a held grant or is
//! refused — there is no unclassified traffic. Operations with a
//! fine-grained selector require BOTH the resource-verb grant AND a
//! matching selector (composition by intersection).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::principal::AgentAlias;

/// The explicit all-instances selector for `allowed_agents`,
/// `allowed_tools`, and `config_write_paths`. Broad access is always this
/// explicit token (or `admin`) — never an empty list.
pub const WILDCARD: &str = "*";

/// Every grantable resource class in the system. One variant per
/// RPC/gateway-addressable surface; a new surface must add its variant here
/// and classify its methods before it can be dispatched.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    strum_macros::Display,
    strum_macros::EnumString,
    strum_macros::EnumIter,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Resource {
    /// Daemon status, health, doctor, and handshake surfaces.
    System,
    /// Agent chat sessions (create, prompt, list, kill, approve).
    Sessions,
    /// Long-term memory entries.
    Memory,
    /// Scheduled jobs and their run history.
    Cron,
    /// Configuration reads and writes (writes additionally gated by
    /// [`ResolvedGrants::may_write_config`]).
    Config,
    /// Agent roster introspection and lifecycle.
    Agents,
    /// Cost and usage accounting.
    Cost,
    /// Skill bundles and skill files.
    Skills,
    /// Personality documents.
    Personality,
    /// Log queries and live subscriptions.
    Logs,
    /// Connected TUI enumeration.
    Tui,
    /// File attachment and workspace listing.
    Files,
    /// Locale catalogs.
    Locales,
    /// First-run quickstart flows.
    Quickstart,
    /// Messaging channel instances.
    Channels,
    /// Model/TTS/transcription provider profiles.
    Providers,
    /// Model catalog and routing.
    Models,
    /// Peer group membership.
    PeerGroups,
    /// Installed plugins and plugin lifecycle.
    Plugins,
    /// Direct tool invocation surfaces.
    Tools,
    /// Standard Operating Procedure authoring, execution, and decision
    /// surfaces.
    Sops,
}

/// What may be done to a [`Resource`].
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    strum_macros::Display,
    strum_macros::EnumString,
    strum_macros::EnumIter,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Verb {
    Create,
    Read,
    Update,
    Delete,
    /// Run/trigger semantics distinct from mutation (prompt a session,
    /// trigger a cron job, execute a skill).
    Execute,
}

/// The merged authorization result for one principal: what it may touch and
/// how. Produced by the shared grant resolver from permission profiles at a
/// specific authorization-policy generation; consumed by dispatch
/// classification, the gateway router, and deep resource checks. Never
/// stored on a `Principal` — consumers re-resolve when the generation
/// changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResolvedGrants {
    /// Full access to everything. Short-circuits all other checks. An
    /// explicit administrator grant dominates the ordinary grant set.
    #[serde(default)]
    pub admin: bool,
    /// Agent aliases this principal may bind or address. Empty = none;
    /// broad access requires the explicit [`WILDCARD`] entry.
    #[serde(default)]
    pub allowed_agents: Vec<AgentAlias>,
    /// Dotted config path prefixes this principal may write. A trailing
    /// `.*` grants the subtree; a bare path grants that exact prop; the
    /// [`WILDCARD`] entry grants every path. Empty = none.
    #[serde(default)]
    pub config_write_paths: Vec<String>,
    /// Tool names this principal may cause an agent to run. Empty = none;
    /// broad access requires the explicit [`WILDCARD`] entry. (The agent's
    /// own tool policy still applies on top.)
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Resource-class grants: which verbs are permitted per resource.
    /// Missing resource = deny.
    #[serde(default)]
    pub resources: BTreeMap<Resource, BTreeSet<Verb>>,
}

impl ResolvedGrants {
    /// The deny-everything grant set (the `Default`).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The allow-everything grant set (admin). Used for the shared-operator
    /// sentinel so legacy single-operator deployments keep today's
    /// behaviour.
    #[must_use]
    pub fn all() -> Self {
        Self {
            admin: true,
            ..Self::default()
        }
    }

    /// Whether this grant set permits `verb` on `resource`.
    #[must_use]
    pub fn permits(&self, resource: Resource, verb: Verb) -> bool {
        if self.admin {
            return true;
        }
        self.resources
            .get(&resource)
            .is_some_and(|verbs| verbs.contains(&verb))
    }

    /// Whether this principal may bind or address the given agent alias.
    /// An empty selector list grants no agents; broad access requires the
    /// explicit [`WILDCARD`] entry or `admin`.
    #[must_use]
    pub fn may_use_agent(&self, alias: &str) -> bool {
        if self.admin {
            return true;
        }
        self.allowed_agents
            .iter()
            .any(|a| a.as_str() == WILDCARD || a.as_str() == alias)
    }

    /// Whether this principal may cause the named tool to run. An empty
    /// selector list grants no tools; broad access requires the explicit
    /// [`WILDCARD`] entry or `admin`.
    #[must_use]
    pub fn may_use_tool(&self, tool: &str) -> bool {
        if self.admin {
            return true;
        }
        self.allowed_tools
            .iter()
            .any(|t| t == WILDCARD || t == tool)
    }

    /// Whether this principal may write the given dotted config path.
    /// A granted `foo.*` covers `foo` itself and every descendant; a
    /// granted bare `foo.bar` covers exactly that prop; the [`WILDCARD`]
    /// entry covers every path. Empty = none.
    #[must_use]
    pub fn may_write_config(&self, path: &str) -> bool {
        if self.admin {
            return true;
        }
        self.config_write_paths.iter().any(|granted| {
            if granted == WILDCARD {
                return true;
            }
            if let Some(prefix) = granted.strip_suffix(".*") {
                path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('.'))
            } else {
                path == granted
            }
        })
    }

    /// Merge another grant set into this one — the deterministic monotonic
    /// union used when one identity maps to multiple permission profiles.
    /// No implicit deny rules, no profile-order precedence; `admin` from
    /// either side dominates.
    pub fn merge(&mut self, other: &Self) {
        self.admin |= other.admin;
        for alias in &other.allowed_agents {
            if !self.allowed_agents.contains(alias) {
                self.allowed_agents.push(alias.clone());
            }
        }
        for path in &other.config_write_paths {
            if !self.config_write_paths.contains(path) {
                self.config_write_paths.push(path.clone());
            }
        }
        for tool in &other.allowed_tools {
            if !self.allowed_tools.contains(tool) {
                self.allowed_tools.push(tool.clone());
            }
        }
        for (resource, verbs) in &other.resources {
            // Verb sets are BTreeSet: already deterministic by construction.
            self.resources.entry(*resource).or_default().extend(verbs);
        }
        // Canonicalize selector order so a merged value is deterministic
        // regardless of the order profiles were merged in (audit output,
        // snapshots, and equality all rely on this, even though the membership
        // checks above are order-insensitive).
        self.allowed_agents.sort();
        self.allowed_tools.sort();
        self.config_write_paths.sort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants_with(resource: Resource, verbs: &[Verb]) -> ResolvedGrants {
        let mut g = ResolvedGrants::none();
        g.resources
            .insert(resource, verbs.iter().copied().collect());
        g
    }

    #[test]
    fn default_denies_everything() {
        let g = ResolvedGrants::none();
        assert!(!g.permits(Resource::Sessions, Verb::Read));
        assert!(!g.may_use_agent("main"));
        assert!(!g.may_write_config("channels.discord"));
        assert!(!g.may_use_tool("shell"));
    }

    #[test]
    fn admin_permits_everything() {
        let g = ResolvedGrants::all();
        assert!(g.permits(Resource::Plugins, Verb::Delete));
        assert!(g.may_use_agent("anything"));
        assert!(g.may_write_config("security.estop"));
        assert!(g.may_use_tool("shell"));
    }

    #[test]
    fn resource_verb_grants_are_exact() {
        let g = grants_with(Resource::Sessions, &[Verb::Create, Verb::Read]);
        assert!(g.permits(Resource::Sessions, Verb::Read));
        assert!(!g.permits(Resource::Sessions, Verb::Delete));
        assert!(!g.permits(Resource::Memory, Verb::Read));
    }

    #[test]
    fn empty_selector_lists_grant_no_instances() {
        // Rev 8: "An empty selector grants no instances" — an empty
        // allowed_tools/allowed_agents/config list never means unrestricted.
        let mut g = grants_with(Resource::Tools, &[Verb::Execute]);
        g.resources
            .insert(Resource::Sessions, [Verb::Create].into());
        assert!(g.permits(Resource::Tools, Verb::Execute));
        assert!(
            !g.may_use_tool("shell"),
            "resource grant alone is not enough"
        );
        assert!(!g.may_use_agent("main"));
        assert!(!g.may_write_config("channels.discord.token"));
    }

    #[test]
    fn broad_access_requires_the_explicit_wildcard() {
        let mut g = ResolvedGrants::none();
        g.allowed_tools = vec![WILDCARD.into()];
        g.allowed_agents = vec![AgentAlias(WILDCARD.into())];
        g.config_write_paths = vec![WILDCARD.into()];
        assert!(g.may_use_tool("shell"));
        assert!(g.may_use_agent("anything"));
        assert!(g.may_write_config("security.estop"));
        assert!(!g.admin, "wildcard selectors do not imply admin");
        assert!(
            !g.permits(Resource::Sessions, Verb::Read),
            "selector wildcards never grant resource verbs"
        );
    }

    #[test]
    fn named_selectors_are_exact_allowlists() {
        let mut g = ResolvedGrants::none();
        g.allowed_tools = vec!["calculator".into()];
        g.allowed_agents = vec![AgentAlias("main".into())];
        assert!(g.may_use_tool("calculator"));
        assert!(!g.may_use_tool("shell"));
        assert!(g.may_use_agent("main"));
        assert!(!g.may_use_agent("ops"));
    }

    #[test]
    fn config_path_wildcards_cover_subtree_only() {
        let mut g = ResolvedGrants::none();
        g.config_write_paths = vec!["channels.*".into(), "cron.enabled".into()];
        assert!(g.may_write_config("channels"));
        assert!(g.may_write_config("channels.discord.main.bot_token"));
        assert!(!g.may_write_config("channels_config"));
        assert!(g.may_write_config("cron.enabled"));
        assert!(!g.may_write_config("cron.max_jobs"));
        assert!(!g.may_write_config("security.estop"));
    }

    #[test]
    fn merge_unions_grants() {
        let mut a = grants_with(Resource::Sessions, &[Verb::Read]);
        a.allowed_agents = vec![AgentAlias("main".into())];
        let mut b = grants_with(Resource::Sessions, &[Verb::Create]);
        b.resources.insert(Resource::Cron, [Verb::Read].into());
        b.allowed_agents = vec![AgentAlias("ops".into()), AgentAlias("main".into())];
        b.allowed_tools = vec!["calculator".into()];
        a.merge(&b);
        assert!(a.permits(Resource::Sessions, Verb::Read));
        assert!(a.permits(Resource::Sessions, Verb::Create));
        assert!(a.permits(Resource::Cron, Verb::Read));
        assert_eq!(a.allowed_agents.len(), 2);
        assert!(a.may_use_tool("calculator"));
    }

    #[test]
    fn merge_is_monotonic_and_admin_dominates() {
        let mut a = ResolvedGrants::none();
        let before = a.clone();
        a.merge(&ResolvedGrants::none());
        assert_eq!(a, before, "merging nothing grants nothing");

        a.merge(&ResolvedGrants::all());
        assert!(a.admin, "admin from either side dominates");
        assert!(a.permits(Resource::Sops, Verb::Delete));
    }

    #[test]
    fn merge_order_is_irrelevant() {
        // Deterministic union AS A VALUE: reverse-order merges must produce
        // equal selector vectors and serialization, not just equal membership.
        let mut a1 = grants_with(Resource::Sessions, &[Verb::Read]);
        a1.allowed_tools = vec!["shell".into(), "calculator".into()];
        a1.config_write_paths = vec!["security".into()];
        let mut b1 = grants_with(Resource::Cron, &[Verb::Execute]);
        b1.allowed_tools = vec!["memory".into()];
        b1.config_write_paths = vec!["users".into(), "oidc".into()];

        let mut ab = a1.clone();
        ab.merge(&b1);
        let mut ba = b1.clone();
        ba.merge(&a1);

        assert_eq!(ab.resources, ba.resources);
        assert_eq!(ab.admin, ba.admin);
        // The canonicalization fix: full selector values are order-independent.
        assert_eq!(ab.allowed_tools, ba.allowed_tools);
        assert_eq!(ab.allowed_agents, ba.allowed_agents);
        assert_eq!(ab.config_write_paths, ba.config_write_paths);
        assert_eq!(ab.allowed_tools, vec!["calculator", "memory", "shell"]);
        assert_eq!(ab.config_write_paths, vec!["oidc", "security", "users"]);
    }

    #[test]
    fn resource_and_verb_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&Resource::PeerGroups).unwrap(),
            "\"peer_groups\""
        );
        assert_eq!(
            serde_json::to_string(&Verb::Execute).unwrap(),
            "\"execute\""
        );
        assert_eq!(Resource::PeerGroups.to_string(), "peer_groups");
        assert_eq!(
            "peer_groups".parse::<Resource>().unwrap(),
            Resource::PeerGroups
        );
    }

    #[test]
    fn grants_roundtrip_json() {
        let mut g = grants_with(Resource::Skills, &[Verb::Read, Verb::Execute]);
        g.config_write_paths = vec!["agents.*".into()];
        g.allowed_tools = vec![WILDCARD.into()];
        let s = serde_json::to_string(&g).unwrap();
        let back: ResolvedGrants = serde_json::from_str(&s).unwrap();
        assert_eq!(g, back);
    }
}
