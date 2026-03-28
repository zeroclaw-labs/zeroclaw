use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for trust scoring
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TrustConfig {
    /// Initial trust score for new domains (default 0.8)
    #[serde(default = "default_initial_score")]
    pub initial_score: f64,
    /// Half-life for trust decay in days (default 30)
    #[serde(default = "default_decay_half_life")]
    pub decay_half_life_days: f64,
    /// Score below which regression is flagged (default 0.5)
    #[serde(default = "default_regression_threshold")]
    pub regression_threshold: f64,
    /// Score penalty per correction event (default 0.05)
    #[serde(default = "default_correction_penalty")]
    pub correction_penalty: f64,
    /// Score boost per success event (default 0.01)
    #[serde(default = "default_success_boost")]
    pub success_boost: f64,
}

fn default_initial_score() -> f64 {
    0.8
}
fn default_decay_half_life() -> f64 {
    30.0
}
fn default_regression_threshold() -> f64 {
    0.5
}
fn default_correction_penalty() -> f64 {
    0.05
}
fn default_success_boost() -> f64 {
    0.01
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            initial_score: default_initial_score(),
            decay_half_life_days: default_decay_half_life(),
            regression_threshold: default_regression_threshold(),
            correction_penalty: default_correction_penalty(),
            success_boost: default_success_boost(),
        }
    }
}

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
