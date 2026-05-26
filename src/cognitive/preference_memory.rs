use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub key: String,
    pub value: f64,
    pub confidence: f64,
    pub source: String,
    pub source_id: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreferenceMemory {
    pub preferences: HashMap<String, Preference>,
}

impl PreferenceMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: &str, value: f64, confidence: f64, source: &str) {
        self.preferences.insert(
            key.to_string(),
            Preference {
                key: key.to_string(),
                value,
                confidence: confidence.clamp(0.0, 1.0),
                source: source.to_string(),
                source_id: source.to_string(),
                timestamp: 0,
            },
        );
    }

    pub fn get(&self, key: &str) -> Option<&Preference> {
        self.preferences.get(key)
    }

    pub fn decay(&mut self, factor: f64) {
        for pref in self.preferences.values_mut() {
            pref.confidence *= factor;
        }
    }

    pub fn high_confidence(&self, threshold: f64) -> Vec<&Preference> {
        self.preferences
            .values()
            .filter(|p| p.confidence >= threshold)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_and_decay() {
        let mut pm = PreferenceMemory::new();
        pm.set("verbosity", 0.8, 0.9, "user");
        let pref = pm.get("verbosity").unwrap();
        assert_eq!(pref.value, 0.8);
        assert_eq!(pref.confidence, 0.9);
        pm.decay(0.5);
        let pref = pm.get("verbosity").unwrap();
        assert!((pref.confidence - 0.45).abs() < f64::EPSILON);
    }

    #[test]
    fn high_confidence_filters() {
        let mut pm = PreferenceMemory::new();
        pm.set("a", 1.0, 0.9, "user");
        pm.set("b", 1.0, 0.3, "inferred");
        assert_eq!(pm.high_confidence(0.5).len(), 1);
    }
}
