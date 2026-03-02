use super::agent_load_tracker::AgentLoadSnapshot;
use crate::config::{AgentLoadBalanceStrategy, DelegateAgentConfig};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;

/// Result of resolving which delegate profile should execute a task.
#[derive(Debug, Clone)]
pub struct AgentSelection {
    pub agent_name: String,
    pub selection_mode: &'static str,
    pub score: usize,
    pub considered: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentSelectionPolicy {
    pub strategy: AgentLoadBalanceStrategy,
    pub inflight_penalty: usize,
    pub recent_selection_penalty: usize,
    pub recent_failure_penalty: usize,
}

impl Default for AgentSelectionPolicy {
    fn default() -> Self {
        Self {
            strategy: AgentLoadBalanceStrategy::Semantic,
            inflight_penalty: 0,
            recent_selection_penalty: 0,
            recent_failure_penalty: 0,
        }
    }
}

/// Select an agent either explicitly (`requested_agent`) or automatically
/// (lexical match over task/context and agent metadata).
pub fn select_agent(
    agents: &HashMap<String, DelegateAgentConfig>,
    requested_agent: Option<&str>,
    task: &str,
    context: &str,
    auto_activate: bool,
    max_active_agents: Option<usize>,
) -> anyhow::Result<AgentSelection> {
    select_agent_with_load(
        agents,
        requested_agent,
        task,
        context,
        auto_activate,
        max_active_agents,
        None,
        AgentSelectionPolicy::default(),
    )
}

/// Select an agent using optional runtime load snapshots and policy controls.
pub fn select_agent_with_load(
    agents: &HashMap<String, DelegateAgentConfig, impl BuildHasher>,
    requested_agent: Option<&str>,
    task: &str,
    context: &str,
    auto_activate: bool,
    max_active_agents: Option<usize>,
    load_snapshots: Option<&HashMap<String, AgentLoadSnapshot>>,
    policy: AgentSelectionPolicy,
) -> anyhow::Result<AgentSelection> {
    let mut names: Vec<String> = agents
        .iter()
        .filter_map(|(name, cfg)| cfg.enabled.then_some(name.clone()))
        .collect();
    names.sort();

    if names.is_empty() {
        anyhow::bail!("No delegate agents are configured (or all are disabled)");
    }

    let requested = requested_agent
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !value.eq_ignore_ascii_case("auto"));

    if let Some(name) = requested {
        if agents.get(name).is_some_and(|cfg| cfg.enabled) {
            return Ok(AgentSelection {
                agent_name: name.to_string(),
                selection_mode: "explicit",
                score: usize::MAX,
                considered: names,
            });
        }

        anyhow::bail!(
            "Unknown agent '{name}' or agent is disabled. Available enabled agents: {}",
            names.join(", ")
        );
    }

    if !auto_activate {
        anyhow::bail!(
            "'agent' is required when automatic activation is disabled. Available agents: {}",
            names.join(", ")
        );
    }

    let query = if context.trim().is_empty() {
        task.to_string()
    } else {
        format!("{task}\n{context}")
    };
    let query_tokens = tokenize(&query);
    let query_lc = query.to_ascii_lowercase();

    let mut ranked: Vec<(String, SelectionScore, AgentLoadSnapshot, usize)> = names
        .iter()
        .filter_map(|name| {
            agents.get(name).map(|agent| {
                let score = selection_score(name, agent, &query_tokens, &query_lc);
                let load = load_snapshots
                    .and_then(|snapshots| snapshots.get(name))
                    .copied()
                    .unwrap_or_default();
                let effective_score = score
                    .summary_score()
                    .saturating_sub(load_penalty(&load, policy));

                (name.clone(), score, load, effective_score)
            })
        })
        .collect();
    ranked.sort_by(
        |(name_a, score_a, load_a, effective_a), (name_b, score_b, load_b, effective_b)| {
            let ordering = match policy.strategy {
                AgentLoadBalanceStrategy::Semantic => {
                    cmp_selection_score(score_a, score_b).then_with(|| effective_b.cmp(effective_a))
                }
                AgentLoadBalanceStrategy::Adaptive => effective_b
                    .cmp(effective_a)
                    .then_with(|| cmp_load_snapshot(load_a, load_b))
                    .then_with(|| cmp_selection_score(score_a, score_b)),
                AgentLoadBalanceStrategy::LeastLoaded => cmp_load_snapshot(load_a, load_b)
                    .then_with(|| cmp_selection_score(score_a, score_b))
                    .then_with(|| effective_b.cmp(effective_a)),
            };
            ordering.then_with(|| name_a.cmp(name_b))
        },
    );

    if let Some(limit) = max_active_agents {
        if limit > 0 && ranked.len() > limit {
            ranked.truncate(limit);
        }
    }

    let Some((selected_name, selected_score, _selected_load, selected_effective_score)) =
        ranked.first().cloned()
    else {
        anyhow::bail!("No selectable agents remain after applying selection constraints");
    };
    let best_score = match policy.strategy {
        AgentLoadBalanceStrategy::Semantic => selected_score.summary_score(),
        AgentLoadBalanceStrategy::Adaptive | AgentLoadBalanceStrategy::LeastLoaded => {
            selected_effective_score
        }
    };
    let selection_mode = match (policy.strategy, selected_score.is_fallback()) {
        (AgentLoadBalanceStrategy::Semantic, true) => "auto_fallback",
        (AgentLoadBalanceStrategy::Semantic, false) => "auto_scored",
        (AgentLoadBalanceStrategy::Adaptive, true) => "auto_balanced_fallback",
        (AgentLoadBalanceStrategy::Adaptive, false) => "auto_balanced",
        (AgentLoadBalanceStrategy::LeastLoaded, true) => "auto_least_loaded_fallback",
        (AgentLoadBalanceStrategy::LeastLoaded, false) => "auto_least_loaded",
    };

    Ok(AgentSelection {
        agent_name: selected_name,
        selection_mode,
        score: best_score,
        considered: ranked.into_iter().map(|(name, _, _, _)| name).collect(),
    })
}

#[derive(Debug, Clone, Copy)]
struct SelectionScore {
    name_match: bool,
    capability_overlap: usize,
    metadata_overlap: usize,
    provider_match: bool,
    model_match: bool,
    priority: i32,
}

impl SelectionScore {
    fn summary_score(self) -> usize {
        let priority = usize::try_from(self.priority.max(0)).unwrap_or(0);
        self.capability_overlap + self.metadata_overlap + priority
    }

    fn is_fallback(self) -> bool {
        !self.name_match
            && self.capability_overlap == 0
            && self.metadata_overlap == 0
            && !self.provider_match
            && !self.model_match
            && self.priority == 0
    }
}

fn cmp_selection_score(a: &SelectionScore, b: &SelectionScore) -> Ordering {
    b.name_match
        .cmp(&a.name_match)
        .then_with(|| b.capability_overlap.cmp(&a.capability_overlap))
        .then_with(|| b.metadata_overlap.cmp(&a.metadata_overlap))
        .then_with(|| b.priority.cmp(&a.priority))
        .then_with(|| b.provider_match.cmp(&a.provider_match))
        .then_with(|| b.model_match.cmp(&a.model_match))
}

fn cmp_load_snapshot(a: &AgentLoadSnapshot, b: &AgentLoadSnapshot) -> Ordering {
    a.in_flight
        .cmp(&b.in_flight)
        .then_with(|| a.recent_failures.cmp(&b.recent_failures))
        .then_with(|| a.recent_assignments.cmp(&b.recent_assignments))
}

fn load_penalty(load: &AgentLoadSnapshot, policy: AgentSelectionPolicy) -> usize {
    load.in_flight
        .saturating_mul(policy.inflight_penalty)
        .saturating_add(
            load.recent_assignments
                .saturating_mul(policy.recent_selection_penalty),
        )
        .saturating_add(
            load.recent_failures
                .saturating_mul(policy.recent_failure_penalty),
        )
}

fn selection_score(
    name: &str,
    agent: &DelegateAgentConfig,
    query_tokens: &HashSet<String>,
    query_lc: &str,
) -> SelectionScore {
    let mut metadata = String::new();
    metadata.push_str(name);
    metadata.push(' ');
    metadata.push_str(&agent.provider);
    metadata.push(' ');
    metadata.push_str(&agent.model);
    metadata.push(' ');
    metadata.push_str(&agent.capabilities.join(" "));
    metadata.push(' ');
    if let Some(system_prompt) = agent.system_prompt.as_deref() {
        metadata.push_str(system_prompt);
    }
    let metadata_tokens = tokenize(&metadata);
    let capabilities_tokens = tokenize(&agent.capabilities.join(" "));

    let metadata_overlap = query_tokens.intersection(&metadata_tokens).count();
    let capability_overlap = query_tokens.intersection(&capabilities_tokens).count();

    let name_lc = name.to_ascii_lowercase();
    let provider_lc = agent.provider.to_ascii_lowercase();
    let model_lc = agent.model.to_ascii_lowercase();

    SelectionScore {
        name_match: !name_lc.is_empty() && query_lc.contains(&name_lc),
        capability_overlap,
        metadata_overlap,
        provider_match: !provider_lc.is_empty() && query_lc.contains(&provider_lc),
        model_match: !model_lc.is_empty() && query_lc.contains(&model_lc),
        priority: agent.priority,
    }
}

fn tokenize(input: &str) -> HashSet<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| part.len() >= 2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> HashMap<String, DelegateAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "openrouter".to_string(),
                model: "claude-sonnet".to_string(),
                system_prompt: Some("Research and summarize technical docs.".to_string()),
                api_key: None,
                enabled: true,
                capabilities: vec!["research".to_string(), "summary".to_string()],
                priority: 0,
                temperature: Some(0.3),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 8,
            },
        );
        agents.insert(
            "coder".to_string(),
            DelegateAgentConfig {
                provider: "openai".to_string(),
                model: "gpt-5.3-codex".to_string(),
                system_prompt: Some("Write and refactor production code.".to_string()),
                api_key: None,
                enabled: true,
                capabilities: vec!["coding".to_string(), "refactor".to_string()],
                priority: 1,
                temperature: Some(0.2),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 8,
            },
        );
        agents
    }

    #[test]
    fn explicit_agent_wins() {
        let selected = select_agent(&agents(), Some("coder"), "anything", "", true, None).unwrap();
        assert_eq!(selected.agent_name, "coder");
        assert_eq!(selected.selection_mode, "explicit");
    }

    #[test]
    fn unknown_explicit_agent_errors() {
        let err = select_agent(&agents(), Some("nope"), "anything", "", true, None).unwrap_err();
        assert!(err.to_string().contains("Unknown agent"));
    }

    #[test]
    fn auto_select_uses_metadata_overlap() {
        let selected = select_agent(
            &agents(),
            None,
            "Please refactor this Rust code and add tests",
            "",
            true,
            None,
        )
        .unwrap();
        assert_eq!(selected.agent_name, "coder");
        assert!(selected.score > 0);
    }

    #[test]
    fn auto_select_respects_disable_flag() {
        let err = select_agent(&agents(), None, "help", "", false, None).unwrap_err();
        assert!(err.to_string().contains("automatic activation is disabled"));
    }

    #[test]
    fn auto_keyword_alias_works() {
        let selected = select_agent(
            &agents(),
            Some("auto"),
            "Summarize documentation findings",
            "",
            true,
            None,
        )
        .unwrap();
        assert_eq!(selected.selection_mode, "auto_scored");
    }

    #[test]
    fn auto_select_respects_priority_when_other_signals_tie() {
        let selected = select_agent(&agents(), None, "help me", "", true, None).unwrap();
        assert_eq!(selected.agent_name, "coder");
    }

    #[test]
    fn disabled_agents_are_not_selectable() {
        let mut pool = agents();
        if let Some(coder) = pool.get_mut("coder") {
            coder.enabled = false;
        }
        let err = select_agent(&pool, Some("coder"), "test", "", true, None).unwrap_err();
        assert!(err.to_string().contains("Unknown agent"));
    }

    #[test]
    fn max_active_agents_limits_auto_pool() {
        let selected =
            select_agent(&agents(), None, "Need coding support", "", true, Some(1)).unwrap();
        assert_eq!(selected.considered.len(), 1);
    }

    #[test]
    fn adaptive_strategy_avoids_overloaded_agent() {
        let mut snapshots = HashMap::new();
        snapshots.insert(
            "coder".to_string(),
            AgentLoadSnapshot {
                in_flight: 4,
                recent_assignments: 6,
                recent_failures: 1,
            },
        );
        snapshots.insert(
            "researcher".to_string(),
            AgentLoadSnapshot {
                in_flight: 0,
                recent_assignments: 0,
                recent_failures: 0,
            },
        );

        let selected = select_agent_with_load(
            &agents(),
            None,
            "please write and refactor rust code",
            "",
            true,
            None,
            Some(&snapshots),
            AgentSelectionPolicy {
                strategy: AgentLoadBalanceStrategy::Adaptive,
                inflight_penalty: 8,
                recent_selection_penalty: 2,
                recent_failure_penalty: 12,
            },
        )
        .unwrap();

        assert_eq!(selected.agent_name, "researcher");
        assert_eq!(selected.selection_mode, "auto_balanced");
    }

    #[test]
    fn least_loaded_strategy_prefers_lightest_agent() {
        let mut snapshots = HashMap::new();
        snapshots.insert(
            "coder".to_string(),
            AgentLoadSnapshot {
                in_flight: 1,
                recent_assignments: 2,
                recent_failures: 0,
            },
        );
        snapshots.insert(
            "researcher".to_string(),
            AgentLoadSnapshot {
                in_flight: 0,
                recent_assignments: 3,
                recent_failures: 0,
            },
        );

        let selected = select_agent_with_load(
            &agents(),
            None,
            "need coding support",
            "",
            true,
            None,
            Some(&snapshots),
            AgentSelectionPolicy {
                strategy: AgentLoadBalanceStrategy::LeastLoaded,
                inflight_penalty: 0,
                recent_selection_penalty: 0,
                recent_failure_penalty: 0,
            },
        )
        .unwrap();

        assert_eq!(selected.agent_name, "researcher");
        assert_eq!(selected.selection_mode, "auto_least_loaded_fallback");
    }
}
