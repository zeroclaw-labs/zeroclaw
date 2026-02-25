use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentRole {
    Primary,
    Advisor,
    Critic,
    Explorer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub role: AgentRole,
    pub beliefs: HashMap<String, f64>,
    pub registered_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusResult {
    pub action: String,
    pub agreement_score: f64,
    pub votes: HashMap<String, f64>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AgentPool {
    agents: HashMap<String, AgentEntry>,
    consensus_history: Vec<ConsensusResult>,
    max_agents: usize,
    history_capacity: usize,
}

impl AgentPool {
    pub fn new(max_agents: usize, history_capacity: usize) -> Self {
        Self {
            agents: HashMap::new(),
            consensus_history: Vec::new(),
            max_agents,
            history_capacity,
        }
    }

    pub fn register_agent(&mut self, name: &str, role: AgentRole) -> bool {
        if self.agents.len() >= self.max_agents || self.agents.contains_key(name) {
            return false;
        }
        let now = Utc::now();
        let entry = AgentEntry {
            name: name.to_string(),
            role,
            beliefs: HashMap::new(),
            registered_at: now,
            last_active: now,
        };
        self.agents.insert(name.to_string(), entry);
        true
    }

    pub fn remove_agent(&mut self, name: &str) -> bool {
        self.agents.remove(name).is_some()
    }

    pub fn update_belief(&mut self, agent_name: &str, key: &str, value: f64) {
        if let Some(agent) = self.agents.get_mut(agent_name) {
            agent.beliefs.insert(key.to_string(), value);
            agent.last_active = Utc::now();
        }
    }

    pub fn broadcast_belief(&mut self, key: &str, value: f64) {
        let now = Utc::now();
        for agent in self.agents.values_mut() {
            agent.beliefs.insert(key.to_string(), value);
            agent.last_active = now;
        }
    }

    pub fn request_consensus(&mut self, action: &str) -> ConsensusResult {
        let mut votes = HashMap::new();
        let action_lower = action.to_lowercase();
        let action_words: Vec<&str> = action_lower.split_whitespace().collect();

        for (name, agent) in &self.agents {
            let score = self.compute_agent_alignment(&action_words, &agent.beliefs);
            votes.insert(name.clone(), score);
        }

        let agreement_score = if votes.is_empty() {
            0.0
        } else {
            votes.values().sum::<f64>() / votes.len() as f64
        };

        let result = ConsensusResult {
            action: action.to_string(),
            agreement_score,
            votes,
            decided_at: Utc::now(),
        };

        self.consensus_history.push(result.clone());
        if self.consensus_history.len() > self.history_capacity {
            self.consensus_history.remove(0);
        }

        result
    }

    pub fn merge_beliefs(&self) -> HashMap<String, f64> {
        let mut sums: HashMap<String, f64> = HashMap::new();
        let mut counts: HashMap<String, usize> = HashMap::new();

        for agent in self.agents.values() {
            for (key, value) in &agent.beliefs {
                *sums.entry(key.clone()).or_default() += value;
                *counts.entry(key.clone()).or_default() += 1;
            }
        }

        sums.into_iter()
            .map(|(key, sum)| {
                let count = counts[&key];
                (key, sum / count as f64)
            })
            .collect()
    }

    pub fn agents_by_role(&self, role: AgentRole) -> Vec<&AgentEntry> {
        self.agents.values().filter(|a| a.role == role).collect()
    }

    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn active_since(&self, since: DateTime<Utc>) -> Vec<&AgentEntry> {
        self.agents
            .values()
            .filter(|a| a.last_active >= since)
            .collect()
    }

    fn compute_agent_alignment(
        &self,
        action_words: &[&str],
        beliefs: &HashMap<String, f64>,
    ) -> f64 {
        if action_words.is_empty() || beliefs.is_empty() {
            return 0.0;
        }

        let mut total = 0.0;
        let mut matches = 0;

        for (key, value) in beliefs {
            let key_lower = key.to_lowercase();
            let key_words: Vec<&str> = key_lower.split_whitespace().collect();
            let matching = action_words
                .iter()
                .filter(|w| w.len() > 2 && key_words.contains(w))
                .count();
            if matching > 0 {
                total += value * matching as f64 / action_words.len().max(1) as f64;
                matches += 1;
            }
        }

        if matches == 0 {
            return 0.0;
        }

        total / f64::from(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pool() -> AgentPool {
        let mut pool = AgentPool::new(5, 10);
        pool.register_agent("zeroclaw_primary", AgentRole::Primary);
        pool.register_agent("zeroclaw_advisor", AgentRole::Advisor);
        pool.register_agent("zeroclaw_critic", AgentRole::Critic);
        pool
    }

    #[test]
    fn register_agent_succeeds() {
        let mut pool = AgentPool::new(5, 10);
        assert!(pool.register_agent("zeroclaw_agent", AgentRole::Primary));
        assert_eq!(pool.agent_count(), 1);
    }

    #[test]
    fn register_agent_when_full_fails() {
        let mut pool = AgentPool::new(2, 10);
        assert!(pool.register_agent("zeroclaw_a", AgentRole::Primary));
        assert!(pool.register_agent("zeroclaw_b", AgentRole::Advisor));
        assert!(!pool.register_agent("zeroclaw_c", AgentRole::Critic));
        assert_eq!(pool.agent_count(), 2);
    }

    #[test]
    fn register_duplicate_name_fails() {
        let mut pool = AgentPool::new(5, 10);
        assert!(pool.register_agent("zeroclaw_agent", AgentRole::Primary));
        assert!(!pool.register_agent("zeroclaw_agent", AgentRole::Advisor));
        assert_eq!(pool.agent_count(), 1);
    }

    #[test]
    fn remove_agent_succeeds() {
        let mut pool = test_pool();
        assert!(pool.remove_agent("zeroclaw_primary"));
        assert_eq!(pool.agent_count(), 2);
    }

    #[test]
    fn remove_nonexistent_agent_returns_false() {
        let mut pool = test_pool();
        assert!(!pool.remove_agent("nonexistent"));
    }

    #[test]
    fn update_belief_sets_value() {
        let mut pool = test_pool();
        pool.update_belief("zeroclaw_primary", "safety", 0.9);
        let merged = pool.merge_beliefs();
        assert!((merged["safety"] - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn broadcast_belief_sets_all_agents() {
        let mut pool = test_pool();
        pool.broadcast_belief("risk_tolerance", 0.5);
        let merged = pool.merge_beliefs();
        assert!((merged["risk_tolerance"] - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn request_consensus_with_multiple_agents() {
        let mut pool = test_pool();
        pool.update_belief("zeroclaw_primary", "deploy system", 0.8);
        pool.update_belief("zeroclaw_advisor", "deploy system", 0.6);
        pool.update_belief("zeroclaw_critic", "deploy system", 0.4);
        let result = pool.request_consensus("deploy system now");
        assert_eq!(result.votes.len(), 3);
        assert!(result.agreement_score >= 0.0);
    }

    #[test]
    fn merge_beliefs_averages_correctly() {
        let mut pool = AgentPool::new(3, 10);
        pool.register_agent("zeroclaw_a", AgentRole::Primary);
        pool.register_agent("zeroclaw_b", AgentRole::Advisor);
        pool.update_belief("zeroclaw_a", "confidence", 0.8);
        pool.update_belief("zeroclaw_b", "confidence", 0.4);
        let merged = pool.merge_beliefs();
        assert!((merged["confidence"] - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn agents_by_role_filters() {
        let pool = test_pool();
        let primaries = pool.agents_by_role(AgentRole::Primary);
        assert_eq!(primaries.len(), 1);
        assert_eq!(primaries[0].name, "zeroclaw_primary");
        let explorers = pool.agents_by_role(AgentRole::Explorer);
        assert_eq!(explorers.len(), 0);
    }

    #[test]
    fn empty_pool_safe_defaults() {
        let mut pool = AgentPool::new(5, 10);
        assert_eq!(pool.agent_count(), 0);
        let merged = pool.merge_beliefs();
        assert!(merged.is_empty());
        let result = pool.request_consensus("anything");
        assert!((result.agreement_score - 0.0).abs() < f64::EPSILON);
        assert!(result.votes.is_empty());
    }

    #[test]
    fn consensus_history_capacity() {
        let mut pool = AgentPool::new(2, 3);
        pool.register_agent("zeroclaw_agent", AgentRole::Primary);
        for i in 0..5 {
            pool.request_consensus(&format!("action_{i}"));
        }
        assert!(pool.consensus_history.len() <= 3);
    }

    #[test]
    fn active_since_filters_by_time() {
        let mut pool = AgentPool::new(5, 10);
        pool.register_agent("zeroclaw_a", AgentRole::Primary);
        let cutoff = Utc::now();
        pool.register_agent("zeroclaw_b", AgentRole::Advisor);
        pool.update_belief("zeroclaw_b", "fresh", 1.0);
        let active = pool.active_since(cutoff);
        assert!(!active.is_empty());
    }

    #[test]
    fn update_belief_on_nonexistent_agent_is_noop() {
        let mut pool = test_pool();
        pool.update_belief("nonexistent", "key", 1.0);
        let merged = pool.merge_beliefs();
        assert!(!merged.contains_key("key"));
    }
}
