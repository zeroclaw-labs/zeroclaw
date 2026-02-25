use super::types::{
    DriftLimits, Preference, PreferenceCategory, PreferenceDelta, PreferenceSnapshot,
};

pub struct PreferenceModel {
    preferences: Vec<Preference>,
    drift_limits: DriftLimits,
    session_drift: f64,
    daily_drift: f64,
    deltas: Vec<PreferenceDelta>,
}

impl PreferenceModel {
    pub fn new(drift_limits: DriftLimits) -> Self {
        Self {
            preferences: Vec::new(),
            drift_limits,
            session_drift: 0.0,
            daily_drift: 0.0,
            deltas: Vec::new(),
        }
    }

    pub fn from_preferences(prefs: Vec<Preference>, drift_limits: DriftLimits) -> Self {
        Self {
            preferences: prefs,
            drift_limits,
            session_drift: 0.0,
            daily_drift: 0.0,
            deltas: Vec::new(),
        }
    }

    pub fn update(
        &mut self,
        key: &str,
        new_value: &str,
        confidence: f64,
        category: PreferenceCategory,
    ) -> Result<(), String> {
        let existing_idx = self.preferences.iter().position(|p| p.key == key);
        let drift = if let Some(idx) = existing_idx {
            if self.preferences[idx].value == new_value {
                return Ok(());
            }
            (self.preferences[idx].confidence - confidence).abs() * 0.5 + 0.01
        } else {
            0.01
        };

        if self.session_drift + drift > self.drift_limits.max_session {
            return Err(format!(
                "Session drift limit exceeded: {:.3} + {:.3} > {:.3}",
                self.session_drift, drift, self.drift_limits.max_session
            ));
        }

        if self.daily_drift + drift > self.drift_limits.max_daily {
            return Err(format!(
                "Daily drift limit exceeded: {:.3} + {:.3} > {:.3}",
                self.daily_drift, drift, self.drift_limits.max_daily
            ));
        }

        self.session_drift += drift;
        self.daily_drift += drift;

        let now = now_timestamp();
        if let Some(idx) = existing_idx {
            let old_value = self.preferences[idx].value.clone();
            let old_confidence = self.preferences[idx].confidence;
            let old_last_updated = self.preferences[idx].last_updated;
            self.preferences[idx]
                .evolution_history
                .push(PreferenceSnapshot {
                    value: old_value.clone(),
                    confidence: old_confidence,
                    timestamp: old_last_updated,
                    reasoning: None,
                });
            self.deltas.push(PreferenceDelta {
                key: key.to_string(),
                old_value,
                new_value: new_value.to_string(),
                old_confidence,
                new_confidence: confidence,
                drift_amount: drift,
                reasoning: None,
                timestamp: now,
            });
            self.preferences[idx].value = new_value.to_string();
            self.preferences[idx].confidence = confidence;
            self.preferences[idx].last_updated = now;
        } else {
            self.preferences.push(Preference {
                key: key.to_string(),
                value: new_value.to_string(),
                confidence,
                category,
                last_updated: now,
                reasoning: None,
                evolution_history: vec![],
            });
        }

        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&Preference> {
        self.preferences.iter().find(|p| p.key == key)
    }

    pub fn preferences(&self) -> &[Preference] {
        &self.preferences
    }

    pub fn drift_limits(&self) -> &DriftLimits {
        &self.drift_limits
    }

    pub fn session_drift(&self) -> f64 {
        self.session_drift
    }

    pub fn daily_drift(&self) -> f64 {
        self.daily_drift
    }

    pub fn reset_session_drift(&mut self) {
        self.session_drift = 0.0;
    }

    pub fn reset_daily_drift(&mut self) {
        self.daily_drift = 0.0;
    }

    pub fn deltas(&self) -> &[PreferenceDelta] {
        &self.deltas
    }

    pub fn clear_deltas(&mut self) -> Vec<PreferenceDelta> {
        std::mem::take(&mut self.deltas)
    }

    pub fn decay_and_gc(&mut self, max_age_secs: u64, min_confidence: f64) -> usize {
        let now = now_timestamp();
        for pref in &mut self.preferences {
            let age = now.saturating_sub(pref.last_updated);
            if age > 0 && max_age_secs > 0 {
                let decay_factor = 1.0 - (age as f64 / max_age_secs as f64).min(1.0);
                pref.confidence *= decay_factor;
            }
        }
        let before = self.preferences.len();
        self.preferences.retain(|p| p.confidence >= min_confidence);
        before - self.preferences.len()
    }
}

fn now_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::types::DriftLimits;

    #[test]
    fn decay_reduces_confidence() {
        let old_ts = now_timestamp().saturating_sub(3600);
        let prefs = vec![Preference {
            key: "tool_affinity:shell".into(),
            value: "preferred".into(),
            confidence: 0.5,
            category: PreferenceCategory::Technical,
            last_updated: old_ts,
            reasoning: None,
            evolution_history: vec![],
        }];
        let mut model = PreferenceModel::from_preferences(prefs, DriftLimits::default());
        let removed = model.decay_and_gc(7200, 0.01);
        assert_eq!(removed, 0);
        let p = model.get("tool_affinity:shell").unwrap();
        assert!(
            p.confidence < 0.5,
            "confidence should have decayed from 0.5"
        );
        assert!(p.confidence > 0.0, "confidence should still be positive");
    }

    #[test]
    fn gc_removes_below_threshold() {
        let prefs = vec![Preference {
            key: "old_pref".into(),
            value: "v".into(),
            confidence: 0.01,
            category: PreferenceCategory::Technical,
            last_updated: now_timestamp(),
            reasoning: None,
            evolution_history: vec![],
        }];
        let mut model = PreferenceModel::from_preferences(prefs, DriftLimits::default());
        let removed = model.decay_and_gc(0, 0.05);
        assert_eq!(removed, 1);
        assert!(model.get("old_pref").is_none());
    }

    #[test]
    fn fresh_preferences_unaffected() {
        let now = now_timestamp();
        let prefs = vec![Preference {
            key: "fresh".into(),
            value: "v".into(),
            confidence: 0.8,
            category: PreferenceCategory::Communication,
            last_updated: now,
            reasoning: None,
            evolution_history: vec![],
        }];
        let mut model = PreferenceModel::from_preferences(prefs, DriftLimits::default());
        let removed = model.decay_and_gc(604_800, 0.05);
        assert_eq!(removed, 0);
        let p = model.get("fresh").unwrap();
        assert!((p.confidence - 0.8).abs() < 0.01);
    }

    #[test]
    fn decay_at_exact_max_age_zeroes_confidence() {
        let max_age = 3600_u64;
        let old_ts = now_timestamp().saturating_sub(max_age);
        let prefs = vec![Preference {
            key: "old".into(),
            value: "v".into(),
            confidence: 0.5,
            category: PreferenceCategory::Technical,
            last_updated: old_ts,
            reasoning: None,
            evolution_history: vec![],
        }];
        let mut model = PreferenceModel::from_preferences(prefs, DriftLimits::default());
        let removed = model.decay_and_gc(max_age, 0.01);
        assert_eq!(
            removed, 1,
            "preference at exact max_age should decay to 0 and be GC'd"
        );
    }
}
