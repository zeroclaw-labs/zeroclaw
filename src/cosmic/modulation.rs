use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GlobalVariable {
    Valence,
    Arousal,
    Confidence,
    Urgency,
    CognitiveLoad,
    SocialPressure,
    Novelty,
    Risk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableState {
    pub variable: GlobalVariable,
    pub value: f64,
    pub baseline: f64,
    pub decay_rate: f64,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModulationSnapshot {
    pub variables: HashMap<GlobalVariable, f64>,
    pub behavioral_bias: BehavioralBias,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BehavioralBias {
    pub exploration_vs_exploitation: f64,
    pub speed_vs_caution: f64,
    pub autonomy_vs_deference: f64,
    pub depth_vs_breadth: f64,
}

impl Default for BehavioralBias {
    fn default() -> Self {
        Self {
            exploration_vs_exploitation: 0.5,
            speed_vs_caution: 0.5,
            autonomy_vs_deference: 0.5,
            depth_vs_breadth: 0.5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmotionalModulator {
    variables: HashMap<GlobalVariable, VariableState>,
}

impl EmotionalModulator {
    pub fn new() -> Self {
        let mut variables = HashMap::new();

        let defaults = [
            (GlobalVariable::Valence, 0.5, 0.5, 0.05),
            (GlobalVariable::Arousal, 0.3, 0.3, 0.08),
            (GlobalVariable::Confidence, 0.5, 0.5, 0.03),
            (GlobalVariable::Urgency, 0.2, 0.2, 0.10),
            (GlobalVariable::CognitiveLoad, 0.3, 0.3, 0.06),
            (GlobalVariable::SocialPressure, 0.2, 0.2, 0.07),
            (GlobalVariable::Novelty, 0.4, 0.4, 0.09),
            (GlobalVariable::Risk, 0.3, 0.3, 0.04),
        ];

        for (var, value, baseline, decay) in defaults {
            variables.insert(
                var,
                VariableState {
                    variable: var,
                    value,
                    baseline,
                    decay_rate: decay,
                    last_updated: Utc::now(),
                },
            );
        }

        Self { variables }
    }

    pub fn set_variable(&mut self, var: GlobalVariable, value: f64) {
        if let Some(state) = self.variables.get_mut(&var) {
            state.value = value.clamp(0.0, 1.0);
            state.last_updated = Utc::now();
        }
    }

    pub fn nudge_variable(&mut self, var: GlobalVariable, delta: f64) {
        if let Some(state) = self.variables.get_mut(&var) {
            state.value = (state.value + delta).clamp(0.0, 1.0);
            state.last_updated = Utc::now();
        }
    }

    pub fn get_variable(&self, var: GlobalVariable) -> f64 {
        self.variables.get(&var).map_or(0.0, |s| s.value)
    }

    pub fn tick(&mut self) {
        for state in self.variables.values_mut() {
            let delta = state.value - state.baseline;
            state.value -= delta * state.decay_rate;
            state.value = state.value.clamp(0.0, 1.0);
        }
    }

    pub fn compute_bias(&self) -> BehavioralBias {
        let novelty = self.get_variable(GlobalVariable::Novelty);
        let confidence = self.get_variable(GlobalVariable::Confidence);
        let urgency = self.get_variable(GlobalVariable::Urgency);
        let risk = self.get_variable(GlobalVariable::Risk);
        let arousal = self.get_variable(GlobalVariable::Arousal);
        let social = self.get_variable(GlobalVariable::SocialPressure);
        let cognitive_load = self.get_variable(GlobalVariable::CognitiveLoad);

        BehavioralBias {
            exploration_vs_exploitation: novelty * 0.6 + (1.0 - confidence) * 0.4,
            speed_vs_caution: urgency * 0.5 + arousal * 0.3 + (1.0 - risk) * 0.2,
            autonomy_vs_deference: confidence * 0.5 + (1.0 - social) * 0.3 + (1.0 - risk) * 0.2,
            depth_vs_breadth: (1.0 - urgency) * 0.4
                + (1.0 - cognitive_load) * 0.3
                + confidence * 0.3,
        }
    }

    pub fn snapshot(&self) -> ModulationSnapshot {
        let variables: HashMap<GlobalVariable, f64> =
            self.variables.iter().map(|(k, v)| (*k, v.value)).collect();

        ModulationSnapshot {
            variables,
            behavioral_bias: self.compute_bias(),
            timestamp: Utc::now(),
        }
    }

    pub fn apply_emotional_input(&mut self, valence: f32, arousal: f32, trust: f32) {
        self.nudge_variable(GlobalVariable::Valence, f64::from(valence) * 0.3);
        self.nudge_variable(GlobalVariable::Arousal, f64::from(arousal) * 0.3);
        self.nudge_variable(GlobalVariable::Confidence, f64::from(trust) * 0.2);
    }

    pub fn apply_free_energy_signal(&mut self, free_energy: f64, surprise_level: f64) {
        self.nudge_variable(GlobalVariable::Urgency, free_energy * 0.2);
        self.nudge_variable(GlobalVariable::Novelty, surprise_level * 0.15);
        self.nudge_variable(GlobalVariable::Risk, free_energy * 0.1);
    }

    pub fn apply_cognitive_load(&mut self, tool_count: usize, context_ratio: f64) {
        let load = (tool_count as f64 * 0.1 + context_ratio * 0.5).min(1.0);
        self.set_variable(GlobalVariable::CognitiveLoad, load);
    }

    pub fn is_overloaded(&self) -> bool {
        self.get_variable(GlobalVariable::CognitiveLoad) > 0.8
    }

    pub fn should_explore(&self) -> bool {
        self.compute_bias().exploration_vs_exploitation > 0.6
    }

    pub fn should_defer(&self) -> bool {
        self.compute_bias().autonomy_vs_deference < 0.4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_modulator_has_all_variables() {
        let m = EmotionalModulator::new();
        assert!(m.get_variable(GlobalVariable::Valence) > 0.0);
        assert!(m.get_variable(GlobalVariable::Arousal) > 0.0);
        assert!(m.get_variable(GlobalVariable::Confidence) > 0.0);
    }

    #[test]
    fn set_variable_clamps() {
        let mut m = EmotionalModulator::new();
        m.set_variable(GlobalVariable::Valence, 1.5);
        assert!((m.get_variable(GlobalVariable::Valence) - 1.0).abs() < f64::EPSILON);
        m.set_variable(GlobalVariable::Valence, -0.5);
        assert!((m.get_variable(GlobalVariable::Valence) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn nudge_variable_incremental() {
        let mut m = EmotionalModulator::new();
        let before = m.get_variable(GlobalVariable::Confidence);
        m.nudge_variable(GlobalVariable::Confidence, 0.1);
        let after = m.get_variable(GlobalVariable::Confidence);
        assert!((after - before - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn tick_decays_toward_baseline() {
        let mut m = EmotionalModulator::new();
        m.set_variable(GlobalVariable::Urgency, 1.0);
        for _ in 0..50 {
            m.tick();
        }
        let v = m.get_variable(GlobalVariable::Urgency);
        let baseline = 0.2;
        assert!(
            (v - baseline).abs() < 0.1,
            "should decay toward baseline {baseline}, got {v}"
        );
    }

    #[test]
    fn behavioral_bias_in_range() {
        let m = EmotionalModulator::new();
        let bias = m.compute_bias();
        assert!(bias.exploration_vs_exploitation >= 0.0 && bias.exploration_vs_exploitation <= 1.0);
        assert!(bias.speed_vs_caution >= 0.0 && bias.speed_vs_caution <= 1.0);
        assert!(bias.autonomy_vs_deference >= 0.0 && bias.autonomy_vs_deference <= 1.0);
        assert!(bias.depth_vs_breadth >= 0.0 && bias.depth_vs_breadth <= 1.0);
    }

    #[test]
    fn high_novelty_increases_exploration() {
        let mut m = EmotionalModulator::new();
        let before = m.compute_bias().exploration_vs_exploitation;
        m.set_variable(GlobalVariable::Novelty, 0.95);
        let after = m.compute_bias().exploration_vs_exploitation;
        assert!(
            after > before,
            "high novelty should increase exploration: before={before} after={after}"
        );
    }

    #[test]
    fn high_urgency_increases_speed() {
        let mut m = EmotionalModulator::new();
        let before = m.compute_bias().speed_vs_caution;
        m.set_variable(GlobalVariable::Urgency, 0.95);
        let after = m.compute_bias().speed_vs_caution;
        assert!(
            after > before,
            "high urgency should increase speed: before={before} after={after}"
        );
    }

    #[test]
    fn high_confidence_increases_autonomy() {
        let mut m = EmotionalModulator::new();
        let before = m.compute_bias().autonomy_vs_deference;
        m.set_variable(GlobalVariable::Confidence, 0.95);
        let after = m.compute_bias().autonomy_vs_deference;
        assert!(
            after > before,
            "high confidence should increase autonomy: before={before} after={after}"
        );
    }

    #[test]
    fn apply_emotional_input_adjusts() {
        let mut m = EmotionalModulator::new();
        let before_valence = m.get_variable(GlobalVariable::Valence);
        m.apply_emotional_input(0.8, 0.6, 0.7);
        let after_valence = m.get_variable(GlobalVariable::Valence);
        assert!(after_valence > before_valence);
    }

    #[test]
    fn apply_free_energy_signal_increases_urgency() {
        let mut m = EmotionalModulator::new();
        let before = m.get_variable(GlobalVariable::Urgency);
        m.apply_free_energy_signal(0.8, 0.5);
        let after = m.get_variable(GlobalVariable::Urgency);
        assert!(after > before);
    }

    #[test]
    fn cognitive_load_overload_detection() {
        let mut m = EmotionalModulator::new();
        assert!(!m.is_overloaded());
        m.apply_cognitive_load(5, 0.9);
        assert!(m.is_overloaded());
    }

    #[test]
    fn snapshot_captures_all_variables() {
        let m = EmotionalModulator::new();
        let snap = m.snapshot();
        assert_eq!(snap.variables.len(), 8);
        assert!(snap.timestamp <= Utc::now());
    }

    #[test]
    fn should_explore_with_high_novelty() {
        let mut m = EmotionalModulator::new();
        m.set_variable(GlobalVariable::Novelty, 0.95);
        m.set_variable(GlobalVariable::Confidence, 0.1);
        assert!(m.should_explore());
    }

    #[test]
    fn should_defer_with_low_confidence_high_social() {
        let mut m = EmotionalModulator::new();
        m.set_variable(GlobalVariable::Confidence, 0.1);
        m.set_variable(GlobalVariable::SocialPressure, 0.9);
        m.set_variable(GlobalVariable::Risk, 0.9);
        assert!(m.should_defer());
    }
}
