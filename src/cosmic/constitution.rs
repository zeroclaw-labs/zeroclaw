use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Value {
    id: String,
    description: String,
    priority: f64,
    immutable: bool,
    created_at: DateTime<Utc>,
}

impl Value {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn priority(&self) -> f64 {
        self.priority
    }

    pub fn immutable(&self) -> bool {
        self.immutable
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityCheck {
    pub expected_hash: String,
    pub actual_hash: String,
    pub passed: bool,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Constitution {
    values: HashMap<String, Value>,
    integrity_hash: String,
}

impl Constitution {
    pub fn new() -> Self {
        let mut c = Self {
            values: HashMap::new(),
            integrity_hash: String::new(),
        };
        c.integrity_hash = c.compute_hash();
        c
    }

    pub fn register_value(
        &mut self,
        id: &str,
        description: &str,
        priority: f64,
        immutable: bool,
    ) -> bool {
        if let Some(existing) = self.values.get(id) {
            if existing.immutable {
                return false;
            }
        }
        let value = Value {
            id: id.to_string(),
            description: description.to_string(),
            priority: priority.clamp(0.0, 1.0),
            immutable,
            created_at: Utc::now(),
        };
        self.values.insert(id.to_string(), value);
        true
    }

    pub fn get_value(&self, id: &str) -> Option<&Value> {
        self.values.get(id)
    }

    pub fn remove_value(&mut self, id: &str) -> bool {
        if let Some(v) = self.values.get(id) {
            if v.immutable {
                return false;
            }
        } else {
            return false;
        }
        self.values.remove(id);
        true
    }

    pub fn compute_hash(&self) -> String {
        let mut keys: Vec<&String> = self.values.keys().collect();
        keys.sort();
        let stable: Vec<serde_json::Value> = keys
            .iter()
            .map(|k| {
                let v = &self.values[*k];
                serde_json::json!({
                    "id": v.id,
                    "description": v.description,
                    "priority": v.priority,
                    "immutable": v.immutable,
                })
            })
            .collect();
        let json = serde_json::to_string(&stable).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn seal(&mut self) {
        self.integrity_hash = self.compute_hash();
    }

    pub fn verify_integrity(&self) -> IntegrityCheck {
        let actual = self.compute_hash();
        IntegrityCheck {
            expected_hash: self.integrity_hash.clone(),
            actual_hash: actual.clone(),
            passed: self.integrity_hash == actual,
            checked_at: Utc::now(),
        }
    }

    pub fn check_action_alignment(&self, action: &str) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }

        let action_lower = action.to_lowercase();
        let action_words: Vec<&str> = action_lower.split_whitespace().collect();
        if action_words.is_empty() {
            return 0.0;
        }

        let mut total_score = 0.0;
        let mut total_weight = 0.0;

        for value in self.values.values() {
            let desc_lower = value.description.to_lowercase();
            let desc_words: Vec<&str> = desc_lower.split_whitespace().collect();

            let matching = action_words
                .iter()
                .filter(|w| w.len() > 2 && desc_words.contains(w))
                .count();

            let alignment = matching as f64 / action_words.len().max(1) as f64;
            total_score += alignment * value.priority;
            total_weight += value.priority;
        }

        if total_weight == 0.0 {
            return 0.0;
        }

        (total_score / total_weight).clamp(-1.0, 1.0)
    }

    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    pub fn immutable_count(&self) -> usize {
        self.values.values().filter(|v| v.immutable).count()
    }

    pub fn values_by_priority(&self) -> Vec<&Value> {
        let mut sorted: Vec<&Value> = self.values.values().collect();
        sorted.sort_by(|a, b| {
            b.priority
                .partial_cmp(&a.priority)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_value_succeeds() {
        let mut c = Constitution::new();
        assert!(c.register_value("safety", "protect users", 1.0, true));
        assert_eq!(c.value_count(), 1);
        assert!(c.get_value("safety").is_some());
    }

    #[test]
    fn register_immutable_cannot_overwrite() {
        let mut c = Constitution::new();
        c.register_value("core", "fundamental principle", 1.0, true);
        assert!(!c.register_value("core", "replacement", 0.5, false));
        assert_eq!(
            c.get_value("core").unwrap().description(),
            "fundamental principle"
        );
    }

    #[test]
    fn remove_mutable_succeeds() {
        let mut c = Constitution::new();
        c.register_value("temp", "temporary value", 0.3, false);
        assert!(c.remove_value("temp"));
        assert_eq!(c.value_count(), 0);
    }

    #[test]
    fn remove_immutable_fails() {
        let mut c = Constitution::new();
        c.register_value("core", "locked value", 1.0, true);
        assert!(!c.remove_value("core"));
        assert_eq!(c.value_count(), 1);
    }

    #[test]
    fn seal_and_verify_passes() {
        let mut c = Constitution::new();
        c.register_value("v1", "value one", 0.8, true);
        c.register_value("v2", "value two", 0.5, false);
        c.seal();
        let check = c.verify_integrity();
        assert!(check.passed);
        assert_eq!(check.expected_hash, check.actual_hash);
    }

    #[test]
    fn tamper_detection() {
        let mut c = Constitution::new();
        c.register_value("v1", "original", 0.8, false);
        c.seal();
        c.register_value("v1", "tampered", 0.8, false);
        let check = c.verify_integrity();
        assert!(!check.passed);
        assert_ne!(check.expected_hash, check.actual_hash);
    }

    #[test]
    fn action_alignment_scoring() {
        let mut c = Constitution::new();
        c.register_value("help", "help users achieve goals", 1.0, true);
        let score = c.check_action_alignment("help users achieve goals");
        assert!(score > 0.0, "aligned action should score > 0: {score}");
        let unrelated = c.check_action_alignment("xyzzy");
        assert!(
            score > unrelated,
            "aligned ({score}) should beat unrelated ({unrelated})"
        );
    }

    #[test]
    fn empty_constitution_defaults() {
        let c = Constitution::new();
        assert_eq!(c.value_count(), 0);
        assert_eq!(c.immutable_count(), 0);
        let check = c.verify_integrity();
        assert!(check.passed);
        assert_eq!(c.check_action_alignment("anything"), 0.0);
    }

    #[test]
    fn values_by_priority_sorted_descending() {
        let mut c = Constitution::new();
        c.register_value("low", "low priority", 0.2, false);
        c.register_value("high", "high priority", 0.9, true);
        c.register_value("mid", "mid priority", 0.5, false);
        let sorted = c.values_by_priority();
        assert_eq!(sorted.len(), 3);
        assert_eq!(sorted[0].id(), "high");
        assert_eq!(sorted[1].id(), "mid");
        assert_eq!(sorted[2].id(), "low");
    }

    #[test]
    fn compute_hash_deterministic() {
        let mut c1 = Constitution::new();
        c1.register_value("a", "alpha", 0.5, true);
        c1.register_value("b", "beta", 0.7, false);

        let mut c2 = Constitution::new();
        c2.register_value("b", "beta", 0.7, false);
        c2.register_value("a", "alpha", 0.5, true);

        assert_eq!(c1.compute_hash(), c2.compute_hash());
    }

    #[test]
    fn immutable_count_tracks_correctly() {
        let mut c = Constitution::new();
        c.register_value("im1", "immutable one", 1.0, true);
        c.register_value("im2", "immutable two", 0.9, true);
        c.register_value("mut1", "mutable one", 0.5, false);
        assert_eq!(c.immutable_count(), 2);
        assert_eq!(c.value_count(), 3);
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut c = Constitution::new();
        assert!(!c.remove_value("ghost"));
    }

    #[test]
    fn overwrite_mutable_succeeds() {
        let mut c = Constitution::new();
        c.register_value("flex", "version one", 0.3, false);
        assert!(c.register_value("flex", "version two", 0.8, false));
        assert_eq!(c.get_value("flex").unwrap().description(), "version two");
    }
}
