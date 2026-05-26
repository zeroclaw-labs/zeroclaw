use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::consciousness::orchestrator::ConsciousnessConfig;
use crate::consciousness::traits::ConsciousnessState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetacognitiveObservation {
    pub tick: u64,
    pub coherence: f64,
    pub attention: f64,
    pub arousal: f64,
    pub valence: f64,
    pub proposals_generated: usize,
    pub proposals_approved: usize,
    pub proposals_vetoed: usize,
    pub debate_rounds_used: usize,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Adjustment {
    pub parameter: String,
    pub old_value: f64,
    pub new_value: f64,
    pub reason: String,
    pub tick: u64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetacognitivePolicy {
    pub min_coherence_threshold: f64,
    pub max_debate_rounds: usize,
    pub arousal_ceiling: f64,
    pub attention_floor: f64,
    pub adjustment_cooldown_ticks: u64,
    pub max_adjustments_per_window: usize,
}

impl Default for MetacognitivePolicy {
    fn default() -> Self {
        Self {
            min_coherence_threshold: 0.3,
            max_debate_rounds: 10,
            arousal_ceiling: 0.95,
            attention_floor: 0.1,
            adjustment_cooldown_ticks: 5,
            max_adjustments_per_window: 3,
        }
    }
}

pub struct MetacognitiveEngine {
    observations: Vec<MetacognitiveObservation>,
    adjustments: Vec<Adjustment>,
    policy: MetacognitivePolicy,
    observation_capacity: usize,
    last_adjustment_tick: u64,
    adjustments_in_window: usize,
    window_start_tick: u64,
}

impl MetacognitiveEngine {
    pub fn new(policy: MetacognitivePolicy) -> Self {
        Self {
            observations: Vec::new(),
            adjustments: Vec::new(),
            policy,
            observation_capacity: 1000,
            last_adjustment_tick: 0,
            adjustments_in_window: 0,
            window_start_tick: 0,
        }
    }

    pub fn observe(
        &mut self,
        state: &ConsciousnessState,
        proposals_generated: usize,
        proposals_approved: usize,
        proposals_vetoed: usize,
        debate_rounds_used: usize,
    ) {
        let obs = MetacognitiveObservation {
            tick: state.tick_count,
            coherence: state.coherence,
            attention: state.phenomenal.attention,
            arousal: state.phenomenal.arousal,
            valence: state.phenomenal.valence,
            proposals_generated,
            proposals_approved,
            proposals_vetoed,
            debate_rounds_used,
            timestamp: Utc::now(),
        };

        if self.observations.len() >= self.observation_capacity {
            self.observations.remove(0);
        }
        self.observations.push(obs);
    }

    pub fn suggest_adjustments(
        &mut self,
        config: &ConsciousnessConfig,
        state: &ConsciousnessState,
    ) -> Vec<Adjustment> {
        let tick = state.tick_count;
        if tick < self.last_adjustment_tick + self.policy.adjustment_cooldown_ticks {
            return Vec::new();
        }

        if tick > self.window_start_tick + 50 {
            self.adjustments_in_window = 0;
            self.window_start_tick = tick;
        }

        if self.adjustments_in_window >= self.policy.max_adjustments_per_window {
            return Vec::new();
        }

        let mut suggestions = Vec::new();

        if let Some(adj) = self.check_debate_efficiency(config, state) {
            suggestions.push(adj);
        }

        if let Some(adj) = self.check_approval_threshold(config, state) {
            suggestions.push(adj);
        }

        if let Some(adj) = self.check_coherence_trend(config, state) {
            suggestions.push(adj);
        }

        suggestions
    }

    pub fn apply_adjustment(&mut self, adjustment: &Adjustment, config: &mut ConsciousnessConfig) {
        match adjustment.parameter.as_str() {
            "debate_rounds" => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let new_val = (adjustment.new_value.max(0.0) as usize)
                    .clamp(1, self.policy.max_debate_rounds);
                config.debate_rounds = new_val;
            }
            "approval_threshold" => {
                config.approval_threshold = adjustment.new_value.clamp(0.3, 0.95);
            }
            "coherence_ema_alpha" => {
                config.coherence_ema_alpha = adjustment.new_value.clamp(0.05, 0.8);
            }
            _ => {}
        }

        self.last_adjustment_tick = adjustment.tick;
        self.adjustments_in_window += 1;
        self.adjustments.push(adjustment.clone());
    }

    pub fn observations(&self) -> &[MetacognitiveObservation] {
        &self.observations
    }

    pub fn adjustments(&self) -> &[Adjustment] {
        &self.adjustments
    }

    pub fn restore(
        &mut self,
        observations: Vec<MetacognitiveObservation>,
        adjustments: Vec<Adjustment>,
    ) {
        self.observations = observations;
        self.adjustments = adjustments;
        if let Some(last) = self.adjustments.last() {
            self.last_adjustment_tick = last.tick;
        }
    }

    pub fn recent_coherence_trend(&self) -> Option<f64> {
        if self.observations.len() < 10 {
            return None;
        }

        let recent: Vec<f64> = self
            .observations
            .iter()
            .rev()
            .take(10)
            .map(|o| o.coherence)
            .collect();
        let older: Vec<f64> = self
            .observations
            .iter()
            .rev()
            .skip(10)
            .take(10)
            .map(|o| o.coherence)
            .collect();

        if older.is_empty() {
            return None;
        }

        let recent_avg = recent.iter().sum::<f64>() / recent.len() as f64;
        let older_avg = older.iter().sum::<f64>() / older.len() as f64;
        Some(recent_avg - older_avg)
    }

    pub fn approval_rate(&self) -> Option<f64> {
        if self.observations.is_empty() {
            return None;
        }

        let recent: Vec<&MetacognitiveObservation> =
            self.observations.iter().rev().take(20).collect();
        let total_generated: usize = recent.iter().map(|o| o.proposals_generated).sum();
        let total_approved: usize = recent.iter().map(|o| o.proposals_approved).sum();

        if total_generated == 0 {
            return None;
        }

        Some(total_approved as f64 / total_generated as f64)
    }

    fn check_debate_efficiency(
        &self,
        config: &ConsciousnessConfig,
        state: &ConsciousnessState,
    ) -> Option<Adjustment> {
        if self.observations.len() < 5 {
            return None;
        }

        let recent: Vec<&MetacognitiveObservation> =
            self.observations.iter().rev().take(5).collect();
        let avg_rounds = recent.iter().map(|o| o.debate_rounds_used).sum::<usize>() as f64 / 5.0;

        if avg_rounds <= 1.0 && config.debate_rounds > 2 {
            return Some(Adjustment {
                parameter: "debate_rounds".to_string(),
                old_value: config.debate_rounds as f64,
                new_value: (config.debate_rounds - 1) as f64,
                reason: format!(
                    "Average debate rounds used ({avg_rounds:.1}) is well below configured ({})",
                    config.debate_rounds
                ),
                tick: state.tick_count,
                timestamp: Utc::now(),
            });
        }

        if avg_rounds >= config.debate_rounds as f64 * 0.9
            && config.debate_rounds < self.policy.max_debate_rounds
        {
            return Some(Adjustment {
                parameter: "debate_rounds".to_string(),
                old_value: config.debate_rounds as f64,
                new_value: (config.debate_rounds + 1) as f64,
                reason: format!(
                    "Debates consistently hitting ceiling ({avg_rounds:.1}/{}) — more rounds may improve consensus",
                    config.debate_rounds
                ),
                tick: state.tick_count,
                timestamp: Utc::now(),
            });
        }

        None
    }

    fn check_approval_threshold(
        &self,
        config: &ConsciousnessConfig,
        state: &ConsciousnessState,
    ) -> Option<Adjustment> {
        let rate = self.approval_rate()?;

        if rate < 0.2 && config.approval_threshold > 0.5 {
            return Some(Adjustment {
                parameter: "approval_threshold".to_string(),
                old_value: config.approval_threshold,
                new_value: config.approval_threshold - 0.05,
                reason: format!(
                    "Approval rate ({rate:.2}) is very low — lowering threshold to increase throughput"
                ),
                tick: state.tick_count,
                timestamp: Utc::now(),
            });
        }

        if rate > 0.95 && config.approval_threshold < 0.9 {
            return Some(Adjustment {
                parameter: "approval_threshold".to_string(),
                old_value: config.approval_threshold,
                new_value: config.approval_threshold + 0.05,
                reason: format!(
                    "Approval rate ({rate:.2}) is near-universal — raising threshold for more selective filtering"
                ),
                tick: state.tick_count,
                timestamp: Utc::now(),
            });
        }

        None
    }

    fn check_coherence_trend(
        &self,
        config: &ConsciousnessConfig,
        state: &ConsciousnessState,
    ) -> Option<Adjustment> {
        let trend = self.recent_coherence_trend()?;

        if trend < -0.1 && config.coherence_ema_alpha > 0.1 {
            return Some(Adjustment {
                parameter: "coherence_ema_alpha".to_string(),
                old_value: config.coherence_ema_alpha,
                new_value: config.coherence_ema_alpha - 0.05,
                reason: format!(
                    "Coherence declining (trend={trend:.3}) — reducing EMA alpha for smoother tracking"
                ),
                tick: state.tick_count,
                timestamp: Utc::now(),
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_state(tick: u64, coherence: f64) -> ConsciousnessState {
        ConsciousnessState {
            tick_count: tick,
            coherence,
            ..Default::default()
        }
    }

    #[test]
    fn observe_records_state() {
        let mut engine = MetacognitiveEngine::new(MetacognitivePolicy::default());
        let state = make_state(1, 0.9);
        engine.observe(&state, 5, 3, 1, 2);
        assert_eq!(engine.observations().len(), 1);
        assert_eq!(engine.observations()[0].proposals_generated, 5);
    }

    #[test]
    fn cooldown_prevents_rapid_adjustments() {
        let mut engine = MetacognitiveEngine::new(MetacognitivePolicy {
            adjustment_cooldown_ticks: 10,
            ..Default::default()
        });
        let config = ConsciousnessConfig::default();
        let state = make_state(1, 0.9);

        engine.last_adjustment_tick = 0;
        let suggestions = engine.suggest_adjustments(&config, &state);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn debate_efficiency_reduces_rounds() {
        let policy = MetacognitivePolicy {
            adjustment_cooldown_ticks: 0,
            ..Default::default()
        };
        let mut engine = MetacognitiveEngine::new(policy);
        let config = ConsciousnessConfig {
            debate_rounds: 5,
            ..Default::default()
        };

        for i in 0..10 {
            let state = make_state(i, 0.9);
            engine.observe(&state, 3, 3, 0, 1);
        }

        let state = make_state(10, 0.9);
        let suggestions = engine.suggest_adjustments(&config, &state);
        assert!(suggestions
            .iter()
            .any(|a| a.parameter == "debate_rounds" && a.new_value < config.debate_rounds as f64));
    }

    #[test]
    fn apply_adjustment_updates_config() {
        let mut engine = MetacognitiveEngine::new(MetacognitivePolicy::default());
        let mut config = ConsciousnessConfig {
            debate_rounds: 5,
            ..Default::default()
        };
        let adj = Adjustment {
            parameter: "debate_rounds".to_string(),
            old_value: 5.0,
            new_value: 3.0,
            reason: "test".to_string(),
            tick: 10,
            timestamp: Utc::now(),
        };

        engine.apply_adjustment(&adj, &mut config);
        assert_eq!(config.debate_rounds, 3);
        assert_eq!(engine.adjustments().len(), 1);
    }

    #[test]
    fn approval_rate_computation() {
        let mut engine = MetacognitiveEngine::new(MetacognitivePolicy::default());
        for i in 0..5 {
            let state = make_state(i, 0.9);
            engine.observe(&state, 10, 8, 2, 2);
        }
        let rate = engine.approval_rate().unwrap();
        assert!((rate - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn coherence_trend_requires_sufficient_data() {
        let mut engine = MetacognitiveEngine::new(MetacognitivePolicy::default());
        let state = make_state(1, 0.9);
        engine.observe(&state, 1, 1, 0, 1);
        assert!(engine.recent_coherence_trend().is_none());
    }

    #[test]
    fn max_adjustments_per_window_enforced() {
        let policy = MetacognitivePolicy {
            adjustment_cooldown_ticks: 0,
            max_adjustments_per_window: 1,
            ..Default::default()
        };
        let mut engine = MetacognitiveEngine::new(policy);
        engine.adjustments_in_window = 1;

        let config = ConsciousnessConfig::default();
        let state = make_state(10, 0.9);
        let suggestions = engine.suggest_adjustments(&config, &state);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn observation_capacity_enforced() {
        let mut engine = MetacognitiveEngine::new(MetacognitivePolicy::default());
        engine.observation_capacity = 5;

        for i in 0..10 {
            let state = make_state(i, 0.9);
            engine.observe(&state, 1, 1, 0, 1);
        }

        assert_eq!(engine.observations().len(), 5);
        assert_eq!(engine.observations()[0].tick, 5);
    }
}
