use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PolicyLayer {
    Learned = 0,
    Contextual = 1,
    Domain = 2,
    Constitutional = 3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub id: String,
    pub layer: PolicyLayer,
    pub domain: String,
    pub condition: String,
    pub action: String,
    pub priority: f64,
    pub weight: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub action: String,
    pub score: f64,
    pub applied_policies: Vec<String>,
    pub conflicts: Vec<(String, String)>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    policies: HashMap<String, Policy>,
    decisions: Vec<PolicyDecision>,
    decision_capacity: usize,
}

impl PolicyEngine {
    pub fn new(decision_capacity: usize) -> Self {
        Self {
            policies: HashMap::new(),
            decisions: Vec::new(),
            decision_capacity,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_policy(
        &mut self,
        id: &str,
        layer: PolicyLayer,
        domain: &str,
        condition: &str,
        action: &str,
        priority: f64,
        weight: f64,
    ) {
        let policy = Policy {
            id: id.to_string(),
            layer,
            domain: domain.to_string(),
            condition: condition.to_string(),
            action: action.to_string(),
            priority: priority.clamp(0.0, 1.0),
            weight: weight.clamp(0.0, 1.0),
            created_at: Utc::now(),
        };
        self.policies.insert(id.to_string(), policy);
    }

    pub fn remove_policy(&mut self, id: &str) -> bool {
        self.policies.remove(id).is_some()
    }

    pub fn evaluate(&mut self, action: &str, context_domain: &str) -> PolicyDecision {
        let matching: Vec<&Policy> = self
            .policies
            .values()
            .filter(|p| p.domain == context_domain)
            .collect();

        if matching.is_empty() {
            let decision = PolicyDecision {
                action: action.to_string(),
                score: 0.0,
                applied_policies: Vec::new(),
                conflicts: Vec::new(),
                decided_at: Utc::now(),
            };
            self.record_decision(decision.clone());
            return decision;
        }

        let action_lower = action.to_lowercase();
        let action_words: Vec<&str> = action_lower.split_whitespace().collect();

        let mut by_layer: HashMap<PolicyLayer, Vec<&Policy>> = HashMap::new();
        for p in &matching {
            by_layer.entry(p.layer).or_default().push(p);
        }

        let mut layers: Vec<PolicyLayer> = by_layer.keys().copied().collect();
        layers.sort();

        let mut applied_policies = Vec::new();
        let mut conflicts = Vec::new();
        let mut total_score = 0.0;
        let mut total_weight = 0.0;

        let mut resolved_actions: HashMap<PolicyLayer, Vec<(String, String)>> = HashMap::new();

        for layer in &layers {
            if let Some(policies) = by_layer.get(layer) {
                let mut layer_actions = Vec::new();
                for p in policies {
                    let alignment = self.compute_alignment(&action_words, &p.condition);
                    if alignment > 0.0 {
                        total_score += alignment * p.weight * p.priority;
                        total_weight += p.weight;
                        applied_policies.push(p.id.clone());
                        layer_actions.push((p.id.clone(), p.action.clone()));
                    }
                }
                resolved_actions.insert(*layer, layer_actions);
            }
        }

        for layer_a in &layers {
            if let Some(actions_a) = resolved_actions.get(layer_a) {
                for layer_b in &layers {
                    if layer_b <= layer_a {
                        continue;
                    }
                    if let Some(actions_b) = resolved_actions.get(layer_b) {
                        for (id_a, act_a) in actions_a {
                            for (id_b, act_b) in actions_b {
                                if act_a != act_b {
                                    conflicts.push((id_a.clone(), id_b.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }

        let score = if total_weight > 0.0 {
            total_score / total_weight
        } else {
            0.0
        };

        let decision = PolicyDecision {
            action: action.to_string(),
            score,
            applied_policies,
            conflicts,
            decided_at: Utc::now(),
        };
        self.record_decision(decision.clone());
        decision
    }

    pub fn check_conflict(&self, id_a: &str, id_b: &str) -> bool {
        let (a, b) = match (self.policies.get(id_a), self.policies.get(id_b)) {
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };

        if a.action == b.action {
            return false;
        }

        let cond_a_lower = a.condition.to_lowercase();
        let cond_b_lower = b.condition.to_lowercase();
        let words_a: Vec<&str> = cond_a_lower.split_whitespace().collect();
        let words_b: Vec<&str> = cond_b_lower.split_whitespace().collect();

        let overlap = words_a
            .iter()
            .filter(|w| w.len() > 2 && words_b.contains(w))
            .count();

        overlap > 0
    }

    pub fn active_policies(&self, domain: &str) -> Vec<&Policy> {
        self.policies
            .values()
            .filter(|p| p.domain == domain)
            .collect()
    }

    pub fn policies_by_layer(&self, layer: PolicyLayer) -> Vec<&Policy> {
        self.policies
            .values()
            .filter(|p| p.layer == layer)
            .collect()
    }

    pub fn policy_count(&self) -> usize {
        self.policies.len()
    }

    pub fn highest_layer_for_domain(&self, domain: &str) -> Option<PolicyLayer> {
        self.policies
            .values()
            .filter(|p| p.domain == domain)
            .map(|p| p.layer)
            .max()
    }

    fn compute_alignment(&self, action_words: &[&str], condition: &str) -> f64 {
        let cond_lower = condition.to_lowercase();
        let cond_words: Vec<&str> = cond_lower.split_whitespace().collect();

        if action_words.is_empty() || cond_words.is_empty() {
            return 0.0;
        }

        let matching = action_words
            .iter()
            .filter(|w| w.len() > 2 && cond_words.contains(w))
            .count();

        matching as f64 / action_words.len().max(1) as f64
    }

    fn record_decision(&mut self, decision: PolicyDecision) {
        self.decisions.push(decision);
        if self.decisions.len() > self.decision_capacity {
            self.decisions.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safety_engine() -> PolicyEngine {
        let mut e = PolicyEngine::new(100);
        e.register_policy(
            "learned_caution",
            PolicyLayer::Learned,
            "safety",
            "risk detected in deployment",
            "proceed with caution",
            0.6,
            0.5,
        );
        e.register_policy(
            "domain_halt",
            PolicyLayer::Domain,
            "safety",
            "risk detected in deployment",
            "halt deployment",
            0.8,
            0.8,
        );
        e.register_policy(
            "constitutional_deny",
            PolicyLayer::Constitutional,
            "safety",
            "risk detected in deployment",
            "deny action",
            1.0,
            1.0,
        );
        e.register_policy(
            "contextual_allow",
            PolicyLayer::Contextual,
            "operations",
            "routine maintenance task",
            "allow execution",
            0.5,
            0.6,
        );
        e
    }

    #[test]
    fn register_policy_and_count() {
        let e = safety_engine();
        assert_eq!(e.policy_count(), 4);
    }

    #[test]
    fn layer_precedence_constitutional_overrides_learned() {
        let e = safety_engine();
        let constitutional = e.policies_by_layer(PolicyLayer::Constitutional);
        let learned = e.policies_by_layer(PolicyLayer::Learned);
        assert_eq!(constitutional.len(), 1);
        assert_eq!(learned.len(), 1);
        assert!(PolicyLayer::Constitutional > PolicyLayer::Learned);
    }

    #[test]
    fn conflict_detection_overlapping_conditions_different_actions() {
        let e = safety_engine();
        assert!(e.check_conflict("learned_caution", "domain_halt"));
        assert!(e.check_conflict("domain_halt", "constitutional_deny"));
    }

    #[test]
    fn no_conflict_when_same_action() {
        let mut e = PolicyEngine::new(100);
        e.register_policy(
            "p1",
            PolicyLayer::Learned,
            "d",
            "deploy system",
            "allow",
            0.5,
            0.5,
        );
        e.register_policy(
            "p2",
            PolicyLayer::Domain,
            "d",
            "deploy system",
            "allow",
            0.8,
            0.8,
        );
        assert!(!e.check_conflict("p1", "p2"));
    }

    #[test]
    fn no_conflict_when_policy_missing() {
        let e = safety_engine();
        assert!(!e.check_conflict("nonexistent", "learned_caution"));
    }

    #[test]
    fn domain_filtering() {
        let e = safety_engine();
        let safety = e.active_policies("safety");
        assert_eq!(safety.len(), 3);
        let ops = e.active_policies("operations");
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn evaluate_with_matching_policies() {
        let mut e = safety_engine();
        let decision = e.evaluate("risk detected in deployment process", "safety");
        assert!(!decision.applied_policies.is_empty());
        assert!(decision.score > 0.0);
    }

    #[test]
    fn evaluate_empty_engine() {
        let mut e = PolicyEngine::new(100);
        let decision = e.evaluate("anything", "any_domain");
        assert!((decision.score - 0.0).abs() < f64::EPSILON);
        assert!(decision.applied_policies.is_empty());
    }

    #[test]
    fn remove_policy() {
        let mut e = safety_engine();
        assert!(e.remove_policy("learned_caution"));
        assert_eq!(e.policy_count(), 3);
        assert!(!e.remove_policy("learned_caution"));
    }

    #[test]
    fn policies_by_layer_filters() {
        let e = safety_engine();
        let domain = e.policies_by_layer(PolicyLayer::Domain);
        assert_eq!(domain.len(), 1);
        assert_eq!(domain[0].id, "domain_halt");
    }

    #[test]
    fn decision_capacity_pruning() {
        let mut e = PolicyEngine::new(3);
        e.register_policy(
            "p1",
            PolicyLayer::Learned,
            "d",
            "test condition",
            "allow",
            0.5,
            0.5,
        );
        for i in 0..5 {
            e.evaluate(&format!("test condition action_{i}"), "d");
        }
        assert!(e.decisions.len() <= 3);
    }

    #[test]
    fn highest_layer_for_domain() {
        let e = safety_engine();
        assert_eq!(
            e.highest_layer_for_domain("safety"),
            Some(PolicyLayer::Constitutional)
        );
        assert_eq!(
            e.highest_layer_for_domain("operations"),
            Some(PolicyLayer::Contextual)
        );
        assert_eq!(e.highest_layer_for_domain("nonexistent"), None);
    }

    #[test]
    fn evaluate_records_conflicts_across_layers() {
        let mut e = safety_engine();
        let decision = e.evaluate("risk detected in deployment process", "safety");
        assert!(
            !decision.conflicts.is_empty(),
            "policies with different actions across layers should produce conflicts"
        );
    }
}
