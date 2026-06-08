//! Runtime-spawned ephemeral sub-agents that inherit their parent
//! agent's identity by default: same UUID, same `SecurityPolicy`, same
//! memory allowlist. A SubAgent run is auditable as a child of the
//! parent and stays inside the parent's permissions envelope.
//!
//! Two spawn sites converge on [`SubAgentSpawn`]: the agent-loop tool
//! `spawn_subagent` and the cron scheduler's `JobType::Agent` dispatch.
//! Sharing the surface keeps permission inheritance, tracing-span
//! shape, and audit attribution uniform.
//!
//! Power-users may narrow a SubAgent's permissions via
//! [`SubAgentOverrides`]; [`SubAgentSpawn::build`] validates each
//! override as a subset of the parent (using
//! [`SecurityPolicy::ensure_no_escalation_beyond`] for the policy and
//! an alias-set containment check for the memory allowlist) and
//! returns `Err` with the originating violation chained on any
//! escalation.
//!
//! The memory allowlist is carried as a set of agent **aliases** (the
//! `[agents.<alias>]` config keys), not backend storage identifiers.
//! Consumers that build an [`AgentScopedMemory`] must resolve aliases
//! to backend identifiers via
//! [`zeroclaw_memory::Memory::ensure_agent_uuid`] first — SQL-backed
//! stores use UUIDs from the `agents` table; Markdown / Qdrant / None
//! use the alias verbatim per the trait default. Holding aliases at
//! this layer means [`SubAgentSpawn::for_agent`] does not need a
//! backend handle to construct.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::sync::Arc;

use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

/// Optional narrowing applied to a SubAgent at spawn time. `None` on
/// every field means "inherit parent verbatim"; `Some(...)` narrows.
/// Each field is independently validated by [`SubAgentSpawn::build`]
/// to reject any value that escalates beyond the parent.
///
/// The default-everything-inherits model means the common case is
/// `SubAgentOverrides::default()` — a no-op.
#[derive(Debug, Clone, Default)]
pub struct SubAgentOverrides {
    /// Override the SubAgent's [`SecurityPolicy`]. Validated as a
    /// subset of the parent via
    /// [`SecurityPolicy::ensure_no_escalation_beyond`].
    pub policy: Option<SecurityPolicy>,
    /// Override the SubAgent's memory allowlist (the set of sibling
    /// agent **aliases** the SubAgent may recall from, as written in
    /// `[agents.<alias>]` keys). Validated as a subset of the
    /// parent's allowlist; any alias here that is not on the parent's
    /// list is rejected.
    ///
    /// These are config-layer aliases, not backend storage
    /// identifiers. Consumers that build an [`AgentScopedMemory`]
    /// must resolve aliases to backend identifiers via
    /// [`zeroclaw_memory::Memory::ensure_agent_uuid`] before passing
    /// them to the wrapper (SQL backends use UUIDs; Markdown / Qdrant
    /// / None use the alias verbatim per the trait default). The
    /// in-tree consumer today is `zeroclaw_memory::create_memory_for_agent`,
    /// which performs the resolution.
    pub allowed_agent_aliases: Option<HashSet<String>>,
}

/// Constructed SubAgent context: bound parent identity, validated
/// child policy, and the resolved memory allowlist.
#[derive(Debug, Clone)]
pub struct SubAgentContext {
    /// The parent agent's alias (e.g. `"researcher"`). SubAgents share
    /// the parent's identity at the data layer (no separate row in the
    /// `agents` table); the distinction between parent and sub-run is
    /// captured at the tracing-span level
    /// (`agent.<alias>.subagent.<run_id>`).
    pub parent_alias: String,
    /// The validated [`SecurityPolicy`] this SubAgent operates under.
    /// Identical to the parent's when `SubAgentOverrides::policy` is
    /// `None`; otherwise a narrowed copy that passed
    /// [`SecurityPolicy::ensure_no_escalation_beyond`].
    pub policy: Arc<SecurityPolicy>,
    /// Resolved memory allowlist as a set of agent **aliases**. The
    /// bound `parent_alias` is always included so the SubAgent always
    /// sees the parent's own rows; the rest is either the parent's
    /// allowlist verbatim or a validated subset.
    ///
    /// See [`SubAgentOverrides::allowed_agent_aliases`] for the
    /// alias-vs-backend-identifier distinction; consumers that build
    /// an [`AgentScopedMemory`] must resolve to backend identifiers
    /// before passing the set to the wrapper.
    pub allowed_agent_aliases: HashSet<String>,
}

/// Builder for a SubAgent spawn. The caller resolves a parent agent
/// from the loaded config; [`Self::build`] applies any narrowing
/// overrides and validates the result.
#[derive(Debug)]
pub struct SubAgentSpawn {
    pub parent_alias: String,
    pub parent_policy: Arc<SecurityPolicy>,
    pub parent_allowed_agent_aliases: HashSet<String>,
}

impl SubAgentSpawn {
    /// Resolve a parent's identity from the loaded config and an
    /// agent alias. Returns `Err` when the alias does not name a
    /// configured agent — the spawn site surfaces a structured
    /// failure instead of invoking the agent loop on a nonexistent
    /// identity.
    pub fn for_agent(config: &Config, agent_alias: &str) -> Result<Self> {
        let agent = config
            .agents
            .get(agent_alias)
            .with_context(|| format!("no agent configured under alias {agent_alias:?}"))?;

        let parent_policy = SecurityPolicy::for_agent(config, agent_alias)
            .map(Arc::new)
            .with_context(|| {
                format!("could not resolve security policy for agent {agent_alias:?}")
            })?;

        let mut parent_allowed_agent_aliases: HashSet<String> = agent
            .workspace
            .read_memory_from
            .iter()
            .map(|alias| alias.as_str().to_string())
            .collect();
        parent_allowed_agent_aliases.insert(agent_alias.to_string());

        Ok(Self {
            parent_alias: agent_alias.to_string(),
            parent_policy,
            parent_allowed_agent_aliases,
        })
    }

    /// Apply `overrides` to the parent's permissions and return a
    /// validated [`SubAgentContext`]. On any escalation, returns
    /// `Err` with the originating violation in the error chain.
    ///
    /// When the caller supplies a policy override, the child inherits
    /// the parent's `PerSenderTracker` so action and cost budgets are
    /// shared between parent and SubAgent runs. Otherwise a SubAgent
    /// could be spawned to bypass the parent's `max_actions_per_hour`
    /// or `max_cost_per_day_cents` ceiling by consuming from a
    /// fresh-zeroed bucket; the inheritance closes that escape. The
    /// no-override path already shares the bucket via
    /// `Arc<SecurityPolicy>` cloning.
    pub fn build(self, overrides: SubAgentOverrides) -> Result<SubAgentContext> {
        let policy = if let Some(mut child_policy) = overrides.policy {
            child_policy
                .ensure_no_escalation_beyond(&self.parent_policy)
                .map_err(|violation| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "violation": violation.to_string(),
                            })),
                        "subagent build refused: policy override escalates beyond parent"
                    );
                    anyhow::Error::msg(format!(
                        "subagent policy override escalates beyond parent: {violation}"
                    ))
                })?;
            // Share the parent's action/cost tracker. `PerSenderTracker`
            // is `Clone` (deep-copy of buckets) but the SubAgent must
            // see the parent's live bucket state, not a frozen
            // snapshot, so steal the parent's tracker by cloning the
            // inner `Arc<SecurityPolicy>` once and assigning the
            // child's `tracker` field from it.
            child_policy.tracker = self.parent_policy.tracker.clone();
            Arc::new(child_policy)
        } else {
            self.parent_policy.clone()
        };

        let allowed_agent_aliases = if let Some(child_allowed) = overrides.allowed_agent_aliases {
            for alias in &child_allowed {
                if !self.parent_allowed_agent_aliases.contains(alias) {
                    anyhow::bail!(
                        "subagent allowlist override contains alias {alias:?} not present on \
                         parent's memory allowlist; SubAgent overrides may only narrow"
                    );
                }
            }
            let mut resolved = child_allowed;
            resolved.insert(self.parent_alias.clone());
            resolved
        } else {
            self.parent_allowed_agent_aliases
        };

        Ok(SubAgentContext {
            parent_alias: self.parent_alias,
            policy,
            allowed_agent_aliases,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use zeroclaw_config::schema::{AliasedAgentConfig, RiskProfileConfig};

    fn config_with_agent(alias: &str) -> Config {
        let mut config = Config::default();
        config
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());
        config.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "default".to_string(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    #[test]
    fn for_agent_resolves_parent_identity_from_config() {
        let config = config_with_agent("alpha");
        let ctx = SubAgentSpawn::for_agent(&config, "alpha")
            .expect("for_agent must succeed for a configured agent")
            .build(SubAgentOverrides::default())
            .expect("inherits-verbatim build must succeed");
        assert_eq!(ctx.parent_alias, "alpha");
        assert!(
            ctx.allowed_agent_aliases.contains("alpha"),
            "an agent always sees its own rows"
        );
    }

    #[test]
    fn for_agent_errors_on_unknown_alias() {
        let err = SubAgentSpawn::for_agent(&Config::default(), "missing")
            .expect_err("unknown alias must error");
        assert!(
            err.to_string().contains("missing"),
            "expected the missing alias in the error, got: {err}"
        );
    }

    #[test]
    fn build_inherits_verbatim_when_overrides_are_default() {
        let config = config_with_agent("alpha");
        let spawn = SubAgentSpawn::for_agent(&config, "alpha").unwrap();
        let parent_policy = spawn.parent_policy.clone();
        let parent_allowlist = spawn.parent_allowed_agent_aliases.clone();

        let ctx = spawn.build(SubAgentOverrides::default()).unwrap();
        assert!(Arc::ptr_eq(&ctx.policy, &parent_policy));
        assert_eq!(ctx.allowed_agent_aliases, parent_allowlist);
    }

    #[test]
    fn build_rejects_policy_override_that_escalates_paths() {
        let config = config_with_agent("alpha");
        let spawn = SubAgentSpawn::for_agent(&config, "alpha").unwrap();

        let mut child_policy = (*spawn.parent_policy).clone();
        // Add an rw root the parent doesn't have — escalation.
        child_policy.allowed_roots.push(PathBuf::from("/secrets"));

        let err = spawn
            .build(SubAgentOverrides {
                policy: Some(child_policy),
                ..SubAgentOverrides::default()
            })
            .expect_err("escalating override must be rejected");
        assert!(
            err.to_string().contains("/secrets"),
            "expected the escalating path in the error chain, got: {err}"
        );
    }

    #[test]
    fn build_rejects_allowlist_override_with_alias_not_on_parent() {
        let config = config_with_agent("alpha");
        let spawn = SubAgentSpawn::for_agent(&config, "alpha").unwrap();

        let mut rogue = HashSet::new();
        rogue.insert("rogue-agent".to_string());

        let err = spawn
            .build(SubAgentOverrides {
                allowed_agent_aliases: Some(rogue),
                ..SubAgentOverrides::default()
            })
            .expect_err("allowlist override with foreign alias must be rejected");
        assert!(
            err.to_string().contains("rogue-agent"),
            "expected the rogue alias in the error chain, got: {err}"
        );
    }

    #[test]
    fn build_accepts_narrowed_allowlist_subset() {
        let config = config_with_agent("alpha");
        let spawn = SubAgentSpawn::for_agent(&config, "alpha").unwrap();

        // Empty subset is still allowed; the bound parent alias is added back.
        let ctx = spawn
            .build(SubAgentOverrides {
                allowed_agent_aliases: Some(HashSet::new()),
                ..SubAgentOverrides::default()
            })
            .expect("narrowing to {} is a valid subset");
        assert_eq!(ctx.allowed_agent_aliases.len(), 1);
        assert!(ctx.allowed_agent_aliases.contains("alpha"));
    }

    #[test]
    fn build_with_override_inherits_parent_action_budget() {
        // SubAgent runs must consume from the parent's action budget
        // so spawning children cannot bypass `max_actions_per_hour`.
        // The override path (caller-supplied policy) is the one with
        // the bug; the inherit-verbatim path is correct by Arc reuse.
        let config = config_with_agent("alpha");
        let spawn = SubAgentSpawn::for_agent(&config, "alpha").unwrap();
        let parent_policy = spawn.parent_policy.clone();

        // Burn the parent's action budget right up to the ceiling so
        // the child's first record_action would push past it.
        for _ in 0..parent_policy.max_actions_per_hour {
            assert!(
                parent_policy.record_action(),
                "parent budget should accept records up to its ceiling"
            );
        }

        // Build a child policy that's a subset of the parent (no
        // escalation) but with the default fresh tracker. The fix
        // copies the parent's tracker into the child so the next
        // record_action sees the parent's exhausted bucket.
        let child_policy = (*parent_policy).clone();
        let ctx = spawn
            .build(SubAgentOverrides {
                policy: Some(child_policy),
                ..SubAgentOverrides::default()
            })
            .expect("inheriting policy as a subset must succeed");

        assert!(
            !ctx.policy.record_action(),
            "child must inherit parent's exhausted action budget; \
             a fresh bucket here means the budget is bypass-able by \
             spawning a SubAgent"
        );
    }
}
