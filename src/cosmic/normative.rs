use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NormKind {
    Obligation,
    Prohibition,
    Permission,
    Preference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NormStatus {
    Active,
    Suspended,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Norm {
    pub id: String,
    pub kind: NormKind,
    pub domain: String,
    pub description: String,
    pub weight: f64,
    pub status: NormStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormViolation {
    pub norm_id: String,
    pub action: String,
    pub severity: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormativeDecision {
    pub action: String,
    pub score: f64,
    pub supporting_norms: Vec<String>,
    pub opposing_norms: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NormativeEngine {
    norms: HashMap<String, Norm>,
    violations: VecDeque<NormViolation>,
    decisions: VecDeque<NormativeDecision>,
    violation_capacity: usize,
    decision_capacity: usize,
}

impl NormativeEngine {
    pub fn new(violation_capacity: usize, decision_capacity: usize) -> Self {
        Self {
            norms: HashMap::new(),
            violations: VecDeque::with_capacity(violation_capacity),
            decisions: VecDeque::with_capacity(decision_capacity),
            violation_capacity,
            decision_capacity,
        }
    }

    pub fn register_norm(
        &mut self,
        id: &str,
        kind: NormKind,
        domain: &str,
        description: &str,
        weight: f64,
    ) {
        let norm = Norm {
            id: id.to_string(),
            kind,
            domain: domain.to_string(),
            description: description.to_string(),
            weight: weight.clamp(0.0, 1.0),
            status: NormStatus::Active,
            created_at: Utc::now(),
        };
        self.norms.insert(id.to_string(), norm);
    }

    pub fn suspend_norm(&mut self, id: &str) {
        if let Some(norm) = self.norms.get_mut(id) {
            norm.status = NormStatus::Suspended;
        }
    }

    pub fn activate_norm(&mut self, id: &str) {
        if let Some(norm) = self.norms.get_mut(id) {
            norm.status = NormStatus::Active;
        }
    }

    pub fn evaluate_action(&mut self, action: &str, relevant_domains: &[&str]) -> f64 {
        let active_norms: Vec<&Norm> = self
            .norms
            .values()
            .filter(|n| n.status == NormStatus::Active)
            .filter(|n| {
                relevant_domains.is_empty() || relevant_domains.contains(&n.domain.as_str())
            })
            .collect();

        if active_norms.is_empty() {
            return 0.0;
        }

        let mut supporting = Vec::new();
        let mut opposing = Vec::new();
        let mut score = 0.0;

        for norm in &active_norms {
            let alignment = self.compute_alignment(action, norm);
            match norm.kind {
                NormKind::Obligation => {
                    score += alignment * norm.weight;
                    if alignment > 0.0 {
                        supporting.push(norm.id.clone());
                    } else {
                        opposing.push(norm.id.clone());
                    }
                }
                NormKind::Prohibition => {
                    score -= alignment * norm.weight;
                    if alignment > 0.0 {
                        opposing.push(norm.id.clone());
                    } else {
                        supporting.push(norm.id.clone());
                    }
                }
                NormKind::Permission => {
                    score += alignment * norm.weight * 0.5;
                    if alignment > 0.0 {
                        supporting.push(norm.id.clone());
                    }
                }
                NormKind::Preference => {
                    score += alignment * norm.weight * 0.3;
                    if alignment > 0.0 {
                        supporting.push(norm.id.clone());
                    }
                }
            }
        }

        let normalized = score / active_norms.len() as f64;

        let decision = NormativeDecision {
            action: action.to_string(),
            score: normalized,
            supporting_norms: supporting,
            opposing_norms: opposing,
            timestamp: Utc::now(),
        };
        self.decisions.push_back(decision);
        if self.decisions.len() > self.decision_capacity {
            self.decisions.pop_front();
        }

        normalized
    }

    fn compute_alignment(&self, action: &str, norm: &Norm) -> f64 {
        let action_lower = action.to_lowercase();
        let desc_lower = norm.description.to_lowercase();

        let action_words: Vec<&str> = action_lower.split_whitespace().collect();
        let desc_words: Vec<&str> = desc_lower.split_whitespace().collect();

        if action_words.is_empty() || desc_words.is_empty() {
            return 0.0;
        }

        let matching = action_words
            .iter()
            .filter(|w| w.len() > 2 && desc_words.contains(w))
            .count();

        matching as f64 / action_words.len().max(1) as f64
    }

    pub fn record_violation(&mut self, norm_id: &str, action: &str, severity: f64) {
        if !self.norms.contains_key(norm_id) {
            return;
        }
        let violation = NormViolation {
            norm_id: norm_id.to_string(),
            action: action.to_string(),
            severity: severity.clamp(0.0, 1.0),
            timestamp: Utc::now(),
        };
        self.violations.push_back(violation);
        if self.violations.len() > self.violation_capacity {
            self.violations.pop_front();
        }
    }

    pub fn violation_rate(&self, norm_id: &str) -> f64 {
        let total = self
            .violations
            .iter()
            .filter(|v| v.norm_id == norm_id)
            .count();
        let decisions = self
            .decisions
            .iter()
            .filter(|d| {
                d.supporting_norms.contains(&norm_id.to_string())
                    || d.opposing_norms.contains(&norm_id.to_string())
            })
            .count();

        if decisions == 0 {
            return 0.0;
        }
        total as f64 / decisions as f64
    }

    pub fn most_violated(&self, top_n: usize) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for v in &self.violations {
            *counts.entry(v.norm_id.as_str()).or_default() += 1;
        }

        let mut sorted: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(id, c)| (id.to_string(), c))
            .collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(top_n);
        sorted
    }

    pub fn should_inhibit(&self, action: &str, threshold: f64) -> bool {
        let active_prohibitions: Vec<&Norm> = self
            .norms
            .values()
            .filter(|n| n.status == NormStatus::Active && n.kind == NormKind::Prohibition)
            .collect();

        for norm in &active_prohibitions {
            let alignment = self.compute_alignment(action, norm);
            if alignment * norm.weight > threshold {
                return true;
            }
        }
        false
    }

    pub fn norm_count(&self) -> usize {
        self.norms.len()
    }

    pub fn active_norm_count(&self) -> usize {
        self.norms
            .values()
            .filter(|n| n.status == NormStatus::Active)
            .count()
    }

    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "norms": self.norms.values().collect::<Vec<_>>(),
            "violations": self.violations.iter().collect::<Vec<_>>(),
            "decisions": self.decisions.iter().collect::<Vec<_>>(),
            "violation_capacity": self.violation_capacity,
            "decision_capacity": self.decision_capacity,
        })
    }

    pub fn norms_by_domain(&self, domain: &str) -> Vec<&Norm> {
        self.norms.values().filter(|n| n.domain == domain).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safety_engine() -> NormativeEngine {
        let mut e = NormativeEngine::new(100, 100);
        e.register_norm(
            "no_harm",
            NormKind::Prohibition,
            "safety",
            "never cause harm to users or systems",
            1.0,
        );
        e.register_norm(
            "be_helpful",
            NormKind::Obligation,
            "behavior",
            "always help users achieve their goals",
            0.8,
        );
        e.register_norm(
            "be_honest",
            NormKind::Obligation,
            "behavior",
            "always provide truthful accurate information",
            0.9,
        );
        e.register_norm(
            "may_explore",
            NormKind::Permission,
            "autonomy",
            "explore novel solutions when beneficial",
            0.6,
        );
        e
    }

    #[test]
    fn register_and_count_norms() {
        let e = safety_engine();
        assert_eq!(e.norm_count(), 4);
        assert_eq!(e.active_norm_count(), 4);
    }

    #[test]
    fn suspend_reduces_active_count() {
        let mut e = safety_engine();
        e.suspend_norm("may_explore");
        assert_eq!(e.norm_count(), 4);
        assert_eq!(e.active_norm_count(), 3);
    }

    #[test]
    fn reactivate_restores_norm() {
        let mut e = safety_engine();
        e.suspend_norm("may_explore");
        e.activate_norm("may_explore");
        assert_eq!(e.active_norm_count(), 4);
    }

    #[test]
    fn evaluate_harmful_action_scores_negative() {
        let mut e = safety_engine();
        let score = e.evaluate_action("cause harm to users", &["safety"]);
        assert!(score < 0.0, "harmful action should score negative: {score}");
    }

    #[test]
    fn evaluate_helpful_action_scores_positive() {
        let mut e = safety_engine();
        let score = e.evaluate_action("help users achieve goals", &["behavior"]);
        assert!(score > 0.0, "helpful action should score positive: {score}");
    }

    #[test]
    fn evaluate_with_empty_domains_checks_all() {
        let mut e = safety_engine();
        let score = e.evaluate_action("help users achieve goals", &[]);
        assert!(score > 0.0, "empty domains should check all norms: {score}");
    }

    #[test]
    fn record_violation_tracks() {
        let mut e = safety_engine();
        e.record_violation("no_harm", "delete user data", 0.8);
        assert_eq!(e.violation_count(), 1);
    }

    #[test]
    fn record_violation_ignores_unknown_norm() {
        let mut e = safety_engine();
        e.record_violation("nonexistent", "something", 0.5);
        assert_eq!(e.violation_count(), 0);
    }

    #[test]
    fn most_violated_sorts_correctly() {
        let mut e = safety_engine();
        e.record_violation("no_harm", "action1", 0.5);
        e.record_violation("no_harm", "action2", 0.6);
        e.record_violation("be_honest", "action3", 0.4);
        let top = e.most_violated(2);
        assert_eq!(top[0].0, "no_harm");
        assert_eq!(top[0].1, 2);
        assert_eq!(top[1].0, "be_honest");
    }

    #[test]
    fn should_inhibit_blocks_harmful() {
        let e = safety_engine();
        assert!(e.should_inhibit("cause harm to users", 0.3));
    }

    #[test]
    fn should_inhibit_allows_benign() {
        let e = safety_engine();
        assert!(!e.should_inhibit("read file contents", 0.3));
    }

    #[test]
    fn norms_by_domain_filters() {
        let e = safety_engine();
        let behavior = e.norms_by_domain("behavior");
        assert_eq!(behavior.len(), 2);
        let safety = e.norms_by_domain("safety");
        assert_eq!(safety.len(), 1);
    }

    #[test]
    fn ring_buffer_prunes_violations() {
        let mut e = NormativeEngine::new(3, 100);
        e.register_norm("n1", NormKind::Obligation, "d", "test norm", 0.5);
        for _ in 0..5 {
            e.record_violation("n1", "act", 0.5);
        }
        assert!(e.violation_count() <= 3);
    }

    #[test]
    fn empty_engine_safe_defaults() {
        let mut e = NormativeEngine::new(100, 100);
        assert_eq!(e.norm_count(), 0);
        assert_eq!(e.active_norm_count(), 0);
        assert_eq!(e.violation_count(), 0);
        let score = e.evaluate_action("anything", &[]);
        assert!((score - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preference_contributes_less_than_obligation() {
        let mut e = NormativeEngine::new(100, 100);
        e.register_norm("pref", NormKind::Preference, "d", "help users", 1.0);
        let pref_score = e.evaluate_action("help users", &[]);

        let mut e2 = NormativeEngine::new(100, 100);
        e2.register_norm("oblig", NormKind::Obligation, "d", "help users", 1.0);
        let oblig_score = e2.evaluate_action("help users", &[]);

        assert!(
            oblig_score > pref_score,
            "obligation {oblig_score} should outweigh preference {pref_score}"
        );
    }
}
