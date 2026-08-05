//! Runtime-spawned ephemeral sub-agents that inherit their parent
//! agent's identity by default: same UUID, same `SecurityPolicy`, same
//! memory allowlist. A SubAgent run is auditable as a child of the
//! parent and stays inside the parent's permissions envelope.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::sync::Arc;

use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

#[derive(Debug, Clone, Default)]
pub struct SubAgentOverrides {
    /// Override the SubAgent's [`SecurityPolicy`]. Validated as a
    /// subset of the parent via
    /// [`SecurityPolicy::ensure_no_escalation_beyond`].
    pub policy: Option<SecurityPolicy>,
    pub allowed_agent_aliases: Option<HashSet<String>>,
}

/// Constructed SubAgent context: bound parent identity, validated
/// child policy, and the resolved memory allowlist.
#[derive(Debug, Clone)]
pub struct SubAgentContext {
    pub parent_alias: String,
    /// The validated [`SecurityPolicy`] this SubAgent operates under.
    /// Identical to the parent's when `SubAgentOverrides::policy` is
    /// `None`; otherwise a narrowed copy that passed
    /// [`SecurityPolicy::ensure_no_escalation_beyond`].
    pub policy: Arc<SecurityPolicy>,
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
    pub fn for_agent(config: &Config, agent_alias: &str) -> Result<Self> {
        // Upfront alias check so a missing-agent failure surfaces with
        // the "no agent configured under alias …" message rather than
        // the policy resolver's less specific "no resolvable
        // risk_profile" wrapping.
        if !config.agents.contains_key(agent_alias) {
            anyhow::bail!("no agent configured under alias {agent_alias:?}");
        }
        let parent_policy = SecurityPolicy::for_agent(config, agent_alias)
            .map(Arc::new)
            .with_context(|| {
                format!("could not resolve security policy for agent {agent_alias:?}")
            })?;
        Self::for_agent_with_policy(config, agent_alias, parent_policy)
    }

    pub fn for_agent_with_policy(
        config: &Config,
        agent_alias: &str,
        parent_policy: Arc<SecurityPolicy>,
    ) -> Result<Self> {
        let agent = config
            .agents
            .get(agent_alias)
            .with_context(|| format!("no agent configured under alias {agent_alias:?}"))?;

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
                risk_profile: "default".into(),
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

    #[test]
    fn for_agent_with_policy_preserves_session_workspace_dir() {
        let config = config_with_agent("alpha");

        // The session cwd is some directory that is NOT
        // `config.agent_workspace_dir("alpha")`. Pick an absolute path
        // that's stable across hosts.
        let session_cwd = PathBuf::from("/tmp/zeroclaw-test-session-cwd-7263");
        let config_workspace = config.agent_workspace_dir("alpha");
        assert_ne!(
            session_cwd, config_workspace,
            "test precondition: session cwd must differ from config workspace"
        );

        // Build the "live" parent policy the way the interactive
        // builders do (config-derived, then session_cwd override).
        let mut live_policy = SecurityPolicy::for_agent(&config, "alpha").unwrap();
        live_policy.workspace_dir = session_cwd.clone();
        let live_policy = Arc::new(live_policy);

        let ctx = SubAgentSpawn::for_agent_with_policy(&config, "alpha", live_policy.clone())
            .expect("for_agent_with_policy must accept a live parent policy")
            .build(SubAgentOverrides::default())
            .expect("inherits-verbatim build must succeed");

        // The child policy must be the same Arc (no clone, no rebuild)
        // and must carry the session cwd through to the loop.
        assert!(
            Arc::ptr_eq(&ctx.policy, &live_policy),
            "default overrides must reuse the parent's Arc, not regenerate"
        );
        assert_eq!(
            ctx.policy.workspace_dir, session_cwd,
            "session cwd must survive the spawn; regression for issue #7263"
        );
    }

    #[test]
    fn for_agent_uses_config_workspace_dir() {
        let config = config_with_agent("alpha");
        let ctx = SubAgentSpawn::for_agent(&config, "alpha")
            .unwrap()
            .build(SubAgentOverrides::default())
            .unwrap();
        assert_eq!(
            ctx.policy.workspace_dir,
            config.agent_workspace_dir("alpha"),
            "for_agent (cron path) must use the per-agent install workspace"
        );
    }
}
