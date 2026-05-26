use num_complex::Complex64;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::consciousness::traits::{
    ActionOutcome, AgentKind, ConsciousnessState, PhenomenalState, Proposal, Verdict, VerdictKind,
};

use super::traits::{EntanglementMap, ProposalSuperposition, QuantumBrain, QuantumPhaseSpace};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumBrainEngine {
    pub entanglement: EntanglementMap,
    pub phase_space: Option<QuantumPhaseSpace>,
    pub coherence_history: Vec<f64>,
    pub annealing_temperature: f64,
    pub decoherence_rate: f64,
    tick: u64,
}

impl QuantumBrainEngine {
    pub fn new() -> Self {
        Self {
            entanglement: EntanglementMap::new(),
            phase_space: None,
            coherence_history: Vec::new(),
            annealing_temperature: 1.0,
            decoherence_rate: 0.05,
            tick: 0,
        }
    }

    pub fn anneal(&mut self) {
        self.annealing_temperature *= 0.95;
        if self.annealing_temperature < 0.01 {
            self.annealing_temperature = 0.01;
        }
    }

    pub fn update_phase_space(&mut self, phenomenal: &PhenomenalState, modulators: [f64; 4]) {
        self.phase_space = Some(QuantumPhaseSpace::from_phenomenal(
            phenomenal,
            modulators,
            self.tick as f64,
        ));
    }

    pub fn quantum_annealing_select(
        &mut self,
        superposition: &mut super::traits::ProposalSuperposition,
        iterations: usize,
        initial_temp: f64,
        rng: &mut dyn rand::RngCore,
    ) -> Option<super::traits::QuantumProposal> {
        if superposition.proposals.is_empty() {
            return None;
        }
        let mut temperature = initial_temp;
        let cooling_rate = if iterations > 1 {
            (0.01_f64 / initial_temp).powf(1.0 / (iterations as f64 - 1.0))
        } else {
            0.01
        };

        for _ in 0..iterations {
            for amp in &mut superposition.quantum_state.amplitudes {
                let perturbation = Complex64::new(
                    (rng.random::<f64>() - 0.5) * temperature * 0.2,
                    (rng.random::<f64>() - 0.5) * temperature * 0.2,
                );
                *amp += perturbation;
            }
            superposition.quantum_state.normalize();
            temperature *= cooling_rate;
            temperature = temperature.max(0.001);
        }

        self.annealing_temperature = temperature;

        let outcome = superposition.quantum_state.measure_and_collapse(rng);
        if outcome < superposition.proposals.len() {
            Some(superposition.proposals[outcome].clone())
        } else {
            superposition.proposals.last().cloned()
        }
    }

    pub fn path_integral_evaluate(&self, scenarios: &[(f64, f64)]) -> f64 {
        if scenarios.is_empty() {
            return 0.0;
        }
        let hbar = 0.1;
        let mut total_amplitude = Complex64::new(0.0, 0.0);

        for (action_cost, risk) in scenarios {
            let s = action_cost + risk;
            let weight = Complex64::from_polar(1.0, s / hbar);
            total_amplitude += weight;
        }
        total_amplitude.norm_sqr() / scenarios.len() as f64
    }
}

impl Default for QuantumBrainEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl QuantumBrain for QuantumBrainEngine {
    fn perceive_quantum(
        &mut self,
        _state: &ConsciousnessState,
        proposals: Vec<Proposal>,
    ) -> ProposalSuperposition {
        self.tick += 1;
        let mut superposition = ProposalSuperposition::from_proposals(proposals);
        superposition.apply_interference();
        superposition
            .quantum_state
            .apply_decoherence(self.decoherence_rate);
        self.coherence_history
            .push(superposition.quantum_state.coherence());
        if self.coherence_history.len() > 1000 {
            self.coherence_history.remove(0);
        }
        superposition
    }

    fn decide_quantum(
        &mut self,
        superposition: &mut ProposalSuperposition,
        rng: &mut dyn rand::RngCore,
    ) -> Option<Proposal> {
        self.anneal();

        if self.annealing_temperature > 0.1 {
            let temp = self.annealing_temperature;
            for amp in &mut superposition.quantum_state.amplitudes {
                let noise = Complex64::new(
                    (rng.random::<f64>() - 0.5) * temp * 0.1,
                    (rng.random::<f64>() - 0.5) * temp * 0.1,
                );
                *amp += noise;
            }
            superposition.quantum_state.normalize();
        }

        superposition.collapse(rng).cloned()
    }

    fn learn_quantum(&mut self, outcome: &ActionOutcome, _superposition: &ProposalSuperposition) {
        if outcome.success {
            self.decoherence_rate = (self.decoherence_rate * 0.95).max(0.001);
        } else {
            self.decoherence_rate = (self.decoherence_rate * 1.05).min(0.5);
        }
    }

    fn quantum_coherence(&self) -> f64 {
        self.coherence_history.last().copied().unwrap_or(0.0)
    }

    fn entanglement_map(&self) -> &EntanglementMap {
        &self.entanglement
    }

    fn phase_space(&self) -> Option<&QuantumPhaseSpace> {
        self.phase_space.as_ref()
    }
}

pub struct QuantumConsciousnessAgent {
    brain: QuantumBrainEngine,
    phenomenal: PhenomenalState,
    last_superposition: Option<ProposalSuperposition>,
}

impl QuantumConsciousnessAgent {
    pub fn new() -> Self {
        Self {
            brain: QuantumBrainEngine::new(),
            phenomenal: PhenomenalState::default(),
            last_superposition: None,
        }
    }
}

impl Default for QuantumConsciousnessAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::consciousness::traits::ConsciousnessAgent for QuantumConsciousnessAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Quantum
    }

    fn perceive(
        &mut self,
        _state: &ConsciousnessState,
        _signals: &[crate::consciousness::bus::BusMessage],
    ) -> Vec<Proposal> {
        vec![]
    }

    fn deliberate(&mut self, proposals: &[Proposal], state: &ConsciousnessState) -> Vec<Verdict> {
        let superposition = self.brain.perceive_quantum(state, proposals.to_vec());

        let probs = superposition.probabilities();
        self.last_superposition = Some(superposition);

        proposals
            .iter()
            .map(|p| {
                let quantum_confidence = probs
                    .iter()
                    .find(|(id, _)| *id == p.id)
                    .map(|(_, prob)| *prob)
                    .unwrap_or(0.0);

                Verdict {
                    voter: AgentKind::Quantum,
                    proposal_id: p.id,
                    kind: if quantum_confidence > 0.3 {
                        VerdictKind::Approve
                    } else {
                        VerdictKind::Reject
                    },
                    confidence: quantum_confidence,
                    objection: if quantum_confidence <= 0.3 {
                        Some(format!(
                            "quantum probability {:.3} below threshold",
                            quantum_confidence
                        ))
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    fn act(&mut self, _approved: &[Proposal]) -> Vec<ActionOutcome> {
        vec![]
    }

    fn reflect(&mut self, outcomes: &[ActionOutcome], _state: &ConsciousnessState) {
        if let Some(ref sup) = self.last_superposition {
            for outcome in outcomes {
                self.brain.learn_quantum(outcome, sup);
            }
        }

        let success_rate = if outcomes.is_empty() {
            0.5
        } else {
            outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64
        };
        self.phenomenal.valence = (success_rate - 0.5) * 2.0;
        self.phenomenal.attention = self.brain.quantum_coherence();
        self.phenomenal.arousal = self.brain.annealing_temperature;
        self.phenomenal.quantum_coherence = self.brain.quantum_coherence();
        self.phenomenal.entanglement_strength = self
            .brain
            .entanglement
            .pairs
            .iter()
            .map(|p| p.strength)
            .sum::<f64>()
            / self.brain.entanglement.pairs.len().max(1) as f64;
        if let Some(ref sup) = self.last_superposition {
            let probs = sup.quantum_state.probabilities();
            let entropy: f64 = probs
                .iter()
                .filter(|&&p| p > 1e-15)
                .map(|p| -p * p.ln())
                .sum();
            self.phenomenal.superposition_entropy = entropy;
        }
    }

    fn vote_weight(&self) -> f64 {
        0.5
    }

    fn phenomenal_state(&self) -> PhenomenalState {
        self.phenomenal
    }

    fn update_phenomenal(&mut self, outcomes: &[ActionOutcome], _state: &ConsciousnessState) {
        let success_rate = if outcomes.is_empty() {
            0.5
        } else {
            outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64
        };
        self.phenomenal.valence = (success_rate - 0.5) * 2.0;
        self.phenomenal.attention = self.brain.quantum_coherence();
        self.phenomenal.quantum_coherence = self.brain.quantum_coherence();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consciousness::traits::Priority;
    use chrono::Utc;

    fn make_proposal(id: u64, action: &str, confidence: f64) -> Proposal {
        Proposal {
            id,
            source: AgentKind::Strategy,
            action: action.to_string(),
            reasoning: "test".to_string(),
            confidence,
            priority: Priority::Normal,
            contradicts: vec![],
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn brain_engine_perceive_and_decide() {
        let mut brain = QuantumBrainEngine::new();
        let state = ConsciousnessState::default();
        let proposals = vec![
            make_proposal(1, "explore", 0.7),
            make_proposal(2, "conserve", 0.3),
        ];
        let mut sup = brain.perceive_quantum(&state, proposals);
        let mut rng = rand::rng();
        let decision = brain.decide_quantum(&mut sup, &mut rng);
        assert!(decision.is_some());
    }

    #[test]
    fn annealing_temperature_decreases() {
        let mut brain = QuantumBrainEngine::new();
        let initial = brain.annealing_temperature;
        brain.anneal();
        assert!(brain.annealing_temperature < initial);
    }

    #[test]
    fn path_integral_nonzero() {
        let brain = QuantumBrainEngine::new();
        let scenarios = vec![(0.5, 0.1), (0.3, 0.2), (0.8, 0.05)];
        let result = brain.path_integral_evaluate(&scenarios);
        assert!(result > 0.0);
    }

    #[test]
    fn quantum_agent_deliberates() {
        use crate::consciousness::traits::ConsciousnessAgent;
        let mut agent = QuantumConsciousnessAgent::new();
        let state = ConsciousnessState::default();
        let proposals = vec![make_proposal(1, "a", 0.8), make_proposal(2, "b", 0.2)];
        let verdicts = agent.deliberate(&proposals, &state);
        assert_eq!(verdicts.len(), 2);
    }

    #[test]
    fn quantum_annealing_selects_proposal() {
        let mut brain = QuantumBrainEngine::new();
        let proposals = vec![
            make_proposal(1, "explore", 0.8),
            make_proposal(2, "conserve", 0.2),
        ];
        let mut sup = crate::quantum::traits::ProposalSuperposition::from_proposals(proposals);
        let mut rng = rand::rng();
        let result = brain.quantum_annealing_select(&mut sup, 20, 1.0, &mut rng);
        assert!(result.is_some());
        assert!(brain.annealing_temperature < 1.0);
    }

    #[test]
    fn quantum_annealing_temperature_decays() {
        let mut brain = QuantumBrainEngine::new();
        let proposals = vec![make_proposal(1, "a", 0.5), make_proposal(2, "b", 0.5)];
        let mut sup = crate::quantum::traits::ProposalSuperposition::from_proposals(proposals);
        let mut rng = rand::rng();
        brain.quantum_annealing_select(&mut sup, 50, 2.0, &mut rng);
        assert!(
            brain.annealing_temperature < 0.1,
            "temperature should decay significantly after 50 iterations"
        );
    }

    #[test]
    fn quantum_annealing_empty_proposals() {
        let mut brain = QuantumBrainEngine::new();
        let mut sup = crate::quantum::traits::ProposalSuperposition::from_proposals(Vec::new());
        let mut rng = rand::rng();
        let result = brain.quantum_annealing_select(&mut sup, 10, 1.0, &mut rng);
        assert!(result.is_none());
    }
}
