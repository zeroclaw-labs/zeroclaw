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
        self.update_with_prediction(
            phenomenal,
            coherence,
            success_rate,
            contradiction_count,
            veto_count,
            tick,
            0.0,
        );
    }

    pub fn update_with_prediction(
        &mut self,
        phenomenal: &PhenomenalState,
        coherence: f64,
        success_rate: f64,
        contradiction_count: usize,
        veto_count: usize,
        tick: u64,
        prediction_error: f64,
    ) {
        let pe = prediction_error.clamp(0.0, 1.0);
        let prediction_accuracy = 1.0 - pe;

        let reward_signal = success_rate * 2.0 - 1.0;
        self.state.dopamine = (self.state.dopamine * (1.0 - self.decay_rate)
            + reward_signal.max(0.0) * 0.25
            + phenomenal.valence.max(0.0) * 0.15
            + prediction_accuracy * 0.1)
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
            + threat_signal * 0.25
            + (1.0 - coherence) * 0.15
            + reward_signal.min(0.0).abs() * 0.05
            + pe * 0.1)
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

    pub fn apply_gpu_metrics(&mut self, metrics: &GpuMetrics) {
        let utilization = metrics.gpu_utilization.clamp(0.0, 1.0);
        let mem_pressure = metrics.memory_utilization.clamp(0.0, 1.0);
        let thermal = (metrics.temperature_celsius / 100.0).clamp(0.0, 1.0);

        self.state.norepinephrine =
            (self.state.norepinephrine + utilization * 0.1 + thermal * 0.05).clamp(0.0, 1.0);

        self.state.cortisol =
            (self.state.cortisol + mem_pressure * 0.08 + (thermal - 0.7).max(0.0) * 0.15)
                .clamp(0.0, 1.0);

        self.state.dopamine =
            (self.state.dopamine + utilization * 0.05 - mem_pressure * 0.03).clamp(0.0, 1.0);

        self.ncn.gain = (self.ncn.gain + utilization * 0.05).clamp(0.0, 1.0);
        self.ncn.precision = (self.ncn.precision - thermal * 0.03).clamp(0.0, 1.0);
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct GpuMetrics {
    pub gpu_utilization: f64,
    pub memory_utilization: f64,
    pub temperature_celsius: f64,
    pub power_watts: f64,
    pub clock_mhz: f64,
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

    #[test]
    fn low_prediction_error_increases_dopamine() {
        let mut engine = NeuromodulationEngine::new(100);
        let initial_dopamine = engine.state().dopamine;
        let phenomenal = PhenomenalState {
            attention: 0.7,
            arousal: 0.5,
            valence: 0.6,
            ..Default::default()
        };
        engine.update_with_prediction(&phenomenal, 0.8, 0.8, 0, 0, 1, 0.05);
        assert!(
            engine.state().dopamine > initial_dopamine,
            "dopamine {} should exceed initial {}",
            engine.state().dopamine,
            initial_dopamine
        );
    }

    #[test]
    fn high_prediction_error_increases_cortisol() {
        let mut engine = NeuromodulationEngine::new(100);
        let initial_cortisol = engine.state().cortisol;
        let phenomenal = PhenomenalState::default();
        engine.update_with_prediction(&phenomenal, 0.5, 0.5, 0, 0, 1, 0.9);
        assert!(
            engine.state().cortisol > initial_cortisol,
            "cortisol {} should exceed initial {}",
            engine.state().cortisol,
            initial_cortisol
        );
    }

    #[test]
    fn gpu_metrics_affect_neuromodulators() {
        let mut engine = NeuromodulationEngine::new(100);
        let initial = *engine.state();
        let metrics = GpuMetrics {
            gpu_utilization: 0.9,
            memory_utilization: 0.8,
            temperature_celsius: 85.0,
            power_watts: 300.0,
            clock_mhz: 1800.0,
        };
        engine.apply_gpu_metrics(&metrics);
        assert!(engine.state().norepinephrine > initial.norepinephrine);
        assert!(engine.state().cortisol > initial.cortisol);
    }

    #[test]
    fn legacy_update_still_works() {
        let mut engine = NeuromodulationEngine::new(100);
        let phenomenal = PhenomenalState::default();
        engine.update(&phenomenal, 0.5, 0.5, 0, 0, 1);
        let s = engine.state();
        assert!(s.dopamine > 0.0 && s.dopamine <= 1.0);
        assert!(s.cortisol >= 0.0 && s.cortisol <= 1.0);
    }
}
