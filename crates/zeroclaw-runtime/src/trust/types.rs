use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use zeroclaw_config::scattered_types::TrustConfig;

/// Per-domain trust score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustScore {
    pub domain: String,
    pub score: f64,
    pub last_updated: DateTime<Utc>,
    pub event_count: u64,
}

/// Types of correction events
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionType {
    UserOverride,
    QualityFailure,
    SopDeviation,
}

/// A logged correction event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionEvent {
    pub domain: String,
    pub correction_type: CorrectionType,
    pub description: String,
    pub timestamp: DateTime<Utc>,
}

/// Alert when regression is detected
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionAlert {
    pub domain: String,
    pub current_score: f64,
    pub threshold: f64,
    pub detected_at: DateTime<Utc>,
}

/// Main trust tracker
pub struct TrustTracker {
    config: TrustConfig,
    scores: HashMap<String, TrustScore>,
    correction_log: Vec<CorrectionEvent>,
}

impl TrustTracker {
    pub fn new(config: TrustConfig) -> Self {
        Self {
            config,
            scores: HashMap::new(),
            correction_log: Vec::new(),
        }
    }

    /// Get current trust score for domain (initializes if missing)
    pub fn get_score(&mut self, domain: &str) -> f64 {
        self.ensure_domain(domain);
        self.scores[domain].score
    }

    /// Record a correction event — reduces trust
    pub fn record_correction(
        &mut self,
        domain: &str,
        correction_type: CorrectionType,
        description: &str,
    ) {
        self.ensure_domain(domain);
        let now = Utc::now();

        let score = self.scores.get_mut(domain).unwrap();
        score.score = (score.score - self.config.correction_penalty).max(0.0);
        score.last_updated = now;
        score.event_count += 1;

        self.correction_log.push(CorrectionEvent {
            domain: domain.to_string(),
            correction_type,
            description: description.to_string(),
            timestamp: now,
        });
    }

    /// Record a success — small boost to trust
    pub fn record_success(&mut self, domain: &str) {
        self.ensure_domain(domain);
        let now = Utc::now();

        let score = self.scores.get_mut(domain).unwrap();
        score.score = (score.score + self.config.success_boost).min(1.0);
        score.last_updated = now;
        score.event_count += 1;
    }

    /// Apply time decay — scores drift toward initial_score
    pub fn apply_decay(&mut self, now: DateTime<Utc>) {
        let half_life_secs = self.config.decay_half_life_days * 86400.0;

        for score in self.scores.values_mut() {
            let elapsed_secs = (now - score.last_updated).num_seconds() as f64;
            if elapsed_secs <= 0.0 {
                continue;
            }

            let decay_factor = 0.5_f64.powf(elapsed_secs / half_life_secs);
            let initial = self.config.initial_score;

            // Decay toward initial_score: score = initial + (score - initial) * decay_factor
            score.score = initial + (score.score - initial) * decay_factor;
            score.last_updated = now;
        }
    }

    /// Check if a domain is in regression
    pub fn check_regression(&mut self, domain: &str) -> Option<RegressionAlert> {
        self.ensure_domain(domain);
        let score = &self.scores[domain];
        if score.score < self.config.regression_threshold {
            Some(RegressionAlert {
                domain: domain.to_string(),
                current_score: score.score,
                threshold: self.config.regression_threshold,
                detected_at: Utc::now(),
            })
        } else {
            None
        }
    }

    /// Get effective autonomy level based on trust score
    /// Reduces by one level if regression detected
    pub fn get_effective_autonomy(&mut self, domain: &str, base_level: &str) -> String {
        if self.check_regression(domain).is_none() {
            return base_level.to_string();
        }

        match base_level {
            "full" => "supervised".to_string(),
            "supervised" => "read_only".to_string(),
            // read_only and unknown levels stay as-is (can't reduce further)
            _ => base_level.to_string(),
        }
    }

    /// Get all correction events for a domain
    pub fn corrections_for_domain(&self, domain: &str) -> Vec<&CorrectionEvent> {
        self.correction_log
            .iter()
            .filter(|e| e.domain == domain)
            .collect()
    }

    /// Get all tracked domains
    pub fn domains(&self) -> Vec<&str> {
        self.scores.keys().map(|s| s.as_str()).collect()
    }

    /// Get all correction events
    pub fn correction_log(&self) -> &[CorrectionEvent] {
        &self.correction_log
    }

    /// Get snapshot of all trust scores
    pub fn snapshot(&self) -> HashMap<String, TrustScore> {
        self.scores.clone()
    }

    /// Access config
    pub fn config(&self) -> &TrustConfig {
        &self.config
    }

    fn ensure_domain(&mut self, domain: &str) {
        if !self.scores.contains_key(domain) {
            self.scores.insert(
                domain.to_string(),
                TrustScore {
                    domain: domain.to_string(),
                    score: self.config.initial_score,
                    last_updated: Utc::now(),
                    event_count: 0,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use zeroclaw_config::scattered_types::TrustConfig;

    fn cfg() -> TrustConfig {
        TrustConfig::default()
    }

    #[test]
    fn new_domain_starts_at_initial_score() {
        let mut t = TrustTracker::new(cfg());
        let score = t.get_score("reasoning");
        assert!((score - 0.8).abs() < 1e-9);
    }

    #[test]
    fn correction_reduces_score() {
        let mut t = TrustTracker::new(cfg());
        t.record_correction("reasoning", CorrectionType::UserOverride, "desc");
        let score = t.get_score("reasoning");
        assert!(
            score < 0.8,
            "score should drop after correction, got {score}"
        );
    }

    #[test]
    fn success_boosts_score() {
        let mut t = TrustTracker::new(cfg());
        // Start below initial_score
        t.record_correction("reasoning", CorrectionType::QualityFailure, "d");
        let before = t.get_score("reasoning");
        t.record_success("reasoning");
        let after = t.get_score("reasoning");
        assert!(after > before, "score should increase after success");
    }

    #[test]
    fn score_clamped_to_zero_on_excess_correction() {
        let mut t = TrustTracker::new(cfg());
        for _ in 0..200 {
            t.record_correction("d", CorrectionType::SopDeviation, "x");
        }
        assert!(t.get_score("d") >= 0.0);
    }

    #[test]
    fn score_clamped_to_one_on_excess_success() {
        let mut t = TrustTracker::new(cfg());
        for _ in 0..200 {
            t.record_success("d");
        }
        assert!(t.get_score("d") <= 1.0);
    }

    #[test]
    fn apply_decay_moves_score_toward_initial() {
        let mut t = TrustTracker::new(cfg());
        for _ in 0..50 {
            t.record_success("d");
        }
        let high = t.get_score("d");
        assert!(high > 0.8);
        let future = Utc::now() + Duration::days(100);
        t.apply_decay(future);
        let decayed = t.get_score("d");
        assert!(decayed < high, "decay should pull score down toward 0.8");
    }

    #[test]
    fn check_regression_below_threshold() {
        let mut t = TrustTracker::new(cfg());
        for _ in 0..20 {
            t.record_correction("d", CorrectionType::UserOverride, "x");
        }
        let score = t.get_score("d");
        // Precondition: if this fires, config defaults changed and the test needs updating.
        assert!(
            score < t.config().regression_threshold,
            "expected score {score} to be below regression_threshold {}",
            t.config().regression_threshold
        );
        assert!(t.check_regression("d").is_some());
    }

    #[test]
    fn get_effective_autonomy_reduces_on_regression() {
        let mut t = TrustTracker::new(cfg());
        for _ in 0..20 {
            t.record_correction("d", CorrectionType::SopDeviation, "x");
        }
        // Precondition: score must be below threshold; if not, config defaults changed.
        assert!(
            t.get_score("d") < t.config().regression_threshold,
            "expected score to be below regression_threshold after 20 corrections"
        );
        assert!(t.check_regression("d").is_some());
        assert_eq!(t.get_effective_autonomy("d", "full"), "supervised");
        assert_eq!(t.get_effective_autonomy("d", "supervised"), "read_only");
        assert_eq!(t.get_effective_autonomy("d", "read_only"), "read_only");
    }
}
