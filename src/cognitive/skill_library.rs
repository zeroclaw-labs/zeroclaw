use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEntry {
    pub name: String,
    pub domain: String,
    pub proficiency: f64,
    pub usage_count: u64,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillLibrary {
    pub skills: HashMap<String, SkillEntry>,
}

impl SkillLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: &str, domain: &str) {
        self.skills.insert(
            name.to_string(),
            SkillEntry {
                name: name.to_string(),
                domain: domain.to_string(),
                proficiency: 0.0,
                usage_count: 0,
                source_id: String::new(),
                timestamp: 0,
                confidence: 0.0,
            },
        );
    }

    pub fn lookup(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.get(name)
    }

    pub fn update_proficiency(&mut self, name: &str, delta: f64) {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.proficiency = (skill.proficiency + delta).clamp(0.0, 1.0);
            skill.usage_count += 1;
        }
    }

    pub fn by_domain(&self, domain: &str) -> Vec<&SkillEntry> {
        self.skills
            .values()
            .filter(|s| s.domain == domain)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut lib = SkillLibrary::new();
        lib.register("code_review", "engineering");
        let skill = lib.lookup("code_review").unwrap();
        assert_eq!(skill.domain, "engineering");
        assert_eq!(skill.proficiency, 0.0);
    }

    #[test]
    fn update_proficiency_clamps() {
        let mut lib = SkillLibrary::new();
        lib.register("debug", "engineering");
        lib.update_proficiency("debug", 0.5);
        lib.update_proficiency("debug", 0.8);
        let skill = lib.lookup("debug").unwrap();
        assert_eq!(skill.proficiency, 1.0);
        assert_eq!(skill.usage_count, 2);
    }

    #[test]
    fn by_domain_filters() {
        let mut lib = SkillLibrary::new();
        lib.register("code", "engineering");
        lib.register("paint", "art");
        lib.register("test", "engineering");
        assert_eq!(lib.by_domain("engineering").len(), 2);
    }
}
