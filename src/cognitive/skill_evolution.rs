use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPerformance {
    pub skill_name: String,
    pub success_rate: f64,
    pub attempts: u64,
    pub successes: u64,
    pub mutations: Vec<String>,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillEvolution {
    pub skills: HashMap<String, SkillPerformance>,
}

impl SkillEvolution {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_outcome(&mut self, skill: &str, success: bool) {
        let perf = self
            .skills
            .entry(skill.to_string())
            .or_insert(SkillPerformance {
                skill_name: skill.to_string(),
                success_rate: 0.0,
                attempts: 0,
                successes: 0,
                mutations: Vec::new(),
                source_id: String::new(),
                timestamp: 0,
                confidence: 0.0,
            });
        perf.attempts += 1;
        if success {
            perf.successes += 1;
        }
        perf.success_rate = perf.successes as f64 / perf.attempts as f64;
    }

    pub fn suggest_mutation(&self, skill: &str) -> Option<String> {
        self.skills.get(skill).and_then(|p| {
            if p.attempts >= 5 && p.success_rate < 0.5 {
                Some(format!("mutate_{}_v{}", skill, p.mutations.len() + 1))
            } else {
                None
            }
        })
    }

    pub fn apply_mutation(&mut self, skill: &str, mutation: String) {
        if let Some(perf) = self.skills.get_mut(skill) {
            perf.mutations.push(mutation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_suggest_mutation() {
        let mut se = SkillEvolution::new();
        for _ in 0..6 {
            se.record_outcome("search", false);
        }
        let perf = se.skills.get("search").unwrap();
        assert_eq!(perf.attempts, 6);
        assert_eq!(perf.success_rate, 0.0);
        let mutation = se.suggest_mutation("search");
        assert!(mutation.is_some());
        assert!(mutation.unwrap().starts_with("mutate_search"));
    }

    #[test]
    fn high_success_no_mutation() {
        let mut se = SkillEvolution::new();
        for _ in 0..10 {
            se.record_outcome("debug", true);
        }
        assert!(se.suggest_mutation("debug").is_none());
    }
}
