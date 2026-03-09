use serde::{Deserialize, Serialize};

use super::traits::PhenomenalState;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NeuromodulatorState {
    pub dopamine: f64,
    pub serotonin: f64,
    pub norepinephrine: f64,
    pub cortisol: f64,
}

impl Default for NeuromodulatorState {
    fn default() -> Self {
        Self {
            dopamine: 0.5,
            serotonin: 0.5,
            norepinephrine: 0.3,
            cortisol: 0.1,
        }
    }
}

impl NeuromodulatorState {
    pub fn as_array(&self) -> [f64; 4] {
        [
            self.dopamine,
            self.serotonin,
            self.norepinephrine,
            self.cortisol,
        ]
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NcnSignals {
    pub precision: f64,
    pub gain: f64,
    pub ffn_gate: f64,
}

impl Default for NcnSignals {
    fn default() -> Self {
        Self {
            precision: 0.5,
            gain: 0.5,
            ffn_gate: 0.5,
        }
    }
}

pub struct NeuromodulationEngine {
    state: NeuromodulatorState,
    ncn: NcnSignals,
    decay_rate: f64,
    history: Vec<NeuromodulatorSnapshot>,
    capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeuromodulatorSnapshot {
    pub tick: u64,
    pub modulators: NeuromodulatorState,
    pub ncn: NcnSignals,
    pub phenomenal: PhenomenalState,
}

impl NeuromodulationEngine {
    pub fn new(capacity: usize) -> Self {
        Self {
            state: NeuromodulatorState::default(),
            ncn: NcnSignals::default(),
            decay_rate: 0.05,
            history: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub fn state(&self) -> &NeuromodulatorState {
        &self.state
    }

    pub fn ncn_signals(&self) -> &NcnSignals {
        &self.ncn
    }

    pub fn history(&self) -> &[NeuromodulatorSnapshot] {
        &self.history
    }

    pub fn update(
        &mut self,
        phenomenal: &PhenomenalState,
        coherence: f64,
        success_rate: f64,
        contradiction_count: usize,
        veto_count: usize,
        tick: u64,
    ) {
        let reward_signal = success_rate * 2.0 - 1.0;
        self.state.dopamine = (self.state.dopamine * (1.0 - self.decay_rate)
            + reward_signal.max(0.0) * 0.3
            + phenomenal.valence.max(0.0) * 0.2)
            .clamp(0.0, 1.0);

        self.state.serotonin = (self.state.serotonin * (1.0 - self.decay_rate)
            + coherence * 0.25
            + (1.0 - contradiction_count.min(5) as f64 / 5.0) * 0.15)
            .clamp(0.0, 1.0);

        let threat_signal = veto_count.min(3) as f64 / 3.0;
        self.state.norepinephrine = (self.state.norepinephrine * (1.0 - self.decay_rate)
            + phenomenal.arousal * 0.3
            + threat_signal * 0.2)
            .clamp(0.0, 1.0);

        self.state.cortisol = (self.state.cortisol * (1.0 - self.decay_rate * 0.5)
            + threat_signal * 0.3
            + (1.0 - coherence) * 0.15
            + reward_signal.min(0.0).abs() * 0.1)
            .clamp(0.0, 1.0);

        self.ncn.precision =
            (coherence * 0.4 + self.state.serotonin * 0.3 + (1.0 - self.state.cortisol) * 0.3)
                .clamp(0.0, 1.0);

        self.ncn.gain = (self.state.norepinephrine * 0.4
            + phenomenal.attention * 0.3
            + self.state.dopamine * 0.3)
            .clamp(0.0, 1.0);

        self.ncn.ffn_gate = (self.state.dopamine * 0.3
            + phenomenal.arousal * 0.3
            + (1.0 - self.state.cortisol * 0.5) * 0.4)
            .clamp(0.0, 1.0);

        let snapshot = NeuromodulatorSnapshot {
            tick,
            modulators: self.state,
            ncn: self.ncn,
            phenomenal: *phenomenal,
        };

        if self.history.len() >= self.capacity {
            self.history.remove(0);
        }
        self.history.push(snapshot);
    }

    pub fn modulate_phenomenal(&self, base: &mut PhenomenalState) {
        base.attention = (base.attention + self.state.norepinephrine * 0.15
            - self.state.cortisol * 0.1)
            .clamp(0.0, 1.0);

        base.arousal = (base.arousal + self.state.dopamine * 0.1 + self.state.norepinephrine * 0.1
            - self.state.serotonin * 0.05)
            .clamp(0.0, 1.0);

        base.valence = (base.valence + self.state.dopamine * 0.15 + self.state.serotonin * 0.1
            - self.state.cortisol * 0.2)
            .clamp(-1.0, 1.0);
    }

    pub fn exploration_drive(&self) -> f64 {
        (self.state.dopamine * 0.5 + self.state.norepinephrine * 0.3 - self.state.cortisol * 0.2)
            .clamp(0.0, 1.0)
    }

    pub fn conservation_drive(&self) -> f64 {
        (self.state.cortisol * 0.5 + (1.0 - self.state.dopamine) * 0.3 + self.state.serotonin * 0.2)
            .clamp(0.0, 1.0)
    }

    pub fn stress_level(&self) -> f64 {
        (self.state.cortisol * 0.6 + self.state.norepinephrine * 0.3 - self.state.serotonin * 0.3)
            .clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_balanced() {
        let engine = NeuromodulationEngine::new(100);
        let s = engine.state();
        assert!(s.dopamine > 0.0 && s.dopamine <= 1.0);
        assert!(s.serotonin > 0.0 && s.serotonin <= 1.0);
        assert!(s.cortisol >= 0.0 && s.cortisol <= 1.0);
    }

    #[test]
    fn positive_outcome_increases_dopamine() {
        let mut engine = NeuromodulationEngine::new(100);
        let initial_dopamine = engine.state().dopamine;
        let phenomenal = PhenomenalState {
            attention: 0.7,
            arousal: 0.5,
            valence: 0.8,
            ..Default::default()
        };
        engine.update(&phenomenal, 0.8, 0.9, 0, 0, 1);
        assert!(engine.state().dopamine > initial_dopamine);
    }

    #[test]
    fn vetoes_increase_cortisol() {
        let mut engine = NeuromodulationEngine::new(100);
        let initial_cortisol = engine.state().cortisol;
        let phenomenal = PhenomenalState::default();
        engine.update(&phenomenal, 0.3, 0.2, 2, 3, 1);
        assert!(engine.state().cortisol > initial_cortisol);
    }

    #[test]
    fn ncn_signals_stay_bounded() {
        let mut engine = NeuromodulationEngine::new(100);
        let phenomenal = PhenomenalState {
            attention: 1.0,
            arousal: 1.0,
            valence: 1.0,
            ..Default::default()
        };
        for i in 0..50 {
            engine.update(&phenomenal, 1.0, 1.0, 0, 0, i);
        }
        let ncn = engine.ncn_signals();
        assert!(ncn.precision >= 0.0 && ncn.precision <= 1.0);
        assert!(ncn.gain >= 0.0 && ncn.gain <= 1.0);
        assert!(ncn.ffn_gate >= 0.0 && ncn.ffn_gate <= 1.0);
    }

    #[test]
    fn history_respects_capacity() {
        let mut engine = NeuromodulationEngine::new(5);
        let p = PhenomenalState::default();
        for i in 0..10 {
            engine.update(&p, 0.5, 0.5, 0, 0, i);
        }
        assert_eq!(engine.history().len(), 5);
    }
}
