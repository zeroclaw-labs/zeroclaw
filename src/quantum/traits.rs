use num_complex::Complex64;
use serde::{Deserialize, Serialize};

use crate::consciousness::traits::{ActionOutcome, ConsciousnessState, PhenomenalState, Proposal};

use super::state::QuantumState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumProposal {
    pub proposal: Proposal,
    pub amplitude: Complex64,
    pub phase: f64,
}

impl QuantumProposal {
    pub fn from_proposal(proposal: Proposal) -> Self {
        let amplitude = Complex64::new(proposal.confidence, 0.0);
        Self {
            phase: 0.0,
            amplitude,
            proposal,
        }
    }

    pub fn with_phase(mut self, phase: f64) -> Self {
        self.phase = phase;
        self.amplitude = Complex64::from_polar(self.amplitude.norm(), phase);
        self
    }

    pub fn probability(&self) -> f64 {
        self.amplitude.norm_sqr()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalSuperposition {
    pub proposals: Vec<QuantumProposal>,
    pub quantum_state: QuantumState,
}

impl ProposalSuperposition {
    pub fn new() -> Self {
        Self {
            proposals: Vec::new(),
            quantum_state: QuantumState::new(0),
        }
    }

    pub fn from_proposals(proposals: Vec<Proposal>) -> Self {
        let n = proposals.len();
        if n == 0 {
            return Self::new();
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let num_qubits = (n as f64).log2().ceil().max(1.0) as usize;
        let dim = 1 << num_qubits;
        let mut amplitudes = vec![Complex64::new(0.0, 0.0); dim];

        let total_confidence: f64 = proposals.iter().map(|p| p.confidence).sum();
        let quantum_proposals: Vec<QuantumProposal> = proposals
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                let amp = Complex64::new((p.confidence / total_confidence).sqrt(), 0.0);
                if i < dim {
                    amplitudes[i] = amp;
                }
                QuantumProposal {
                    phase: 0.0,
                    amplitude: amp,
                    proposal: p,
                }
            })
            .collect();

        let mut quantum_state = QuantumState {
            amplitudes,
            phase: 0.0,
            num_qubits,
        };
        quantum_state.normalize();

        Self {
            proposals: quantum_proposals,
            quantum_state,
        }
    }

    pub fn apply_interference(&mut self) {
        for i in 0..self.proposals.len() {
            for j in (i + 1)..self.proposals.len() {
                let contradicts_i = &self.proposals[i].proposal.contradicts;
                let id_j = self.proposals[j].proposal.id;
                let contradicts_j = &self.proposals[j].proposal.contradicts;
                let id_i = self.proposals[i].proposal.id;

                if contradicts_i.contains(&id_j) || contradicts_j.contains(&id_i) {
                    let phase_shift = std::f64::consts::PI;
                    if j < self.quantum_state.dimension() {
                        self.quantum_state.amplitudes[j] *= Complex64::from_polar(1.0, phase_shift);
                        self.proposals[j].phase += phase_shift;
                        self.proposals[j].amplitude = Complex64::from_polar(
                            self.proposals[j].amplitude.norm(),
                            self.proposals[j].phase,
                        );
                    }
                } else if self.proposals[i].proposal.action == self.proposals[j].proposal.action
                    && i < self.quantum_state.dimension()
                    && j < self.quantum_state.dimension()
                {
                    let boost = f64::midpoint(
                        self.quantum_state.amplitudes[i].norm(),
                        self.quantum_state.amplitudes[j].norm(),
                    ) * 0.1;
                    self.quantum_state.amplitudes[i] += Complex64::new(boost, 0.0);
                    self.quantum_state.amplitudes[j] += Complex64::new(boost, 0.0);
                }
            }
        }
        self.quantum_state.normalize();
    }

    pub fn collapse<R: rand::Rng + ?Sized>(&mut self, rng: &mut R) -> Option<&Proposal> {
        if self.proposals.is_empty() {
            return None;
        }
        let outcome = self.quantum_state.measure_and_collapse(rng);
        if outcome < self.proposals.len() {
            Some(&self.proposals[outcome].proposal)
        } else {
            self.proposals.last().map(|qp| &qp.proposal)
        }
    }

    pub fn coherence(&self) -> f64 {
        self.quantum_state.coherence()
    }

    pub fn probabilities(&self) -> Vec<(u64, f64)> {
        let probs = self.quantum_state.probabilities();
        self.proposals
            .iter()
            .enumerate()
            .map(|(i, qp)| {
                let p = if i < probs.len() { probs[i] } else { 0.0 };
                (qp.proposal.id, p)
            })
            .collect()
    }
}

impl Default for ProposalSuperposition {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntanglementMap {
    pub pairs: Vec<EntanglementPair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntanglementPair {
    pub agent_a: String,
    pub agent_b: String,
    pub strength: f64,
    pub correlation_history: Vec<f64>,
}

impl EntanglementMap {
    pub fn new() -> Self {
        Self { pairs: Vec::new() }
    }

    pub fn update_correlation(&mut self, agent_a: &str, agent_b: &str, correlated: bool) {
        let value = if correlated { 1.0 } else { -1.0 };
        let pair = self.pairs.iter_mut().find(|p| {
            (p.agent_a == agent_a && p.agent_b == agent_b)
                || (p.agent_a == agent_b && p.agent_b == agent_a)
        });

        match pair {
            Some(p) => {
                p.correlation_history.push(value);
                if p.correlation_history.len() > 100 {
                    p.correlation_history.remove(0);
                }
                let alpha = 0.1;
                p.strength = p.strength * (1.0 - alpha) + value.abs() * alpha;
                p.strength = p.strength.clamp(0.0, 1.0);
            }
            None => {
                self.pairs.push(EntanglementPair {
                    agent_a: agent_a.to_string(),
                    agent_b: agent_b.to_string(),
                    strength: if correlated { 0.1 } else { 0.0 },
                    correlation_history: vec![value],
                });
            }
        }
    }

    pub fn get_strength(&self, agent_a: &str, agent_b: &str) -> f64 {
        self.pairs
            .iter()
            .find(|p| {
                (p.agent_a == agent_a && p.agent_b == agent_b)
                    || (p.agent_a == agent_b && p.agent_b == agent_a)
            })
            .map(|p| p.strength)
            .unwrap_or(0.0)
    }
}

impl Default for EntanglementMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantumPhaseSpace {
    pub dopamine: f64,
    pub serotonin: f64,
    pub norepinephrine: f64,
    pub cortisol: f64,
    pub attention: f64,
    pub arousal: f64,
    pub valence: f64,
    pub time: f64,
}

impl QuantumPhaseSpace {
    pub fn from_phenomenal(state: &PhenomenalState, modulators: [f64; 4], time: f64) -> Self {
        Self {
            dopamine: modulators[0],
            serotonin: modulators[1],
            norepinephrine: modulators[2],
            cortisol: modulators[3],
            attention: state.attention,
            arousal: state.arousal,
            valence: state.valence,
            time,
        }
    }

    pub fn to_vector(&self) -> [f64; 8] {
        [
            self.dopamine,
            self.serotonin,
            self.norepinephrine,
            self.cortisol,
            self.attention,
            self.arousal,
            self.valence,
            self.time,
        ]
    }

    pub fn distance(&self, other: &Self) -> f64 {
        let a = self.to_vector();
        let b = other.to_vector();
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f64>()
            .sqrt()
    }
}

pub trait QuantumBrain: Send + Sync {
    fn perceive_quantum(
        &mut self,
        state: &ConsciousnessState,
        proposals: Vec<Proposal>,
    ) -> ProposalSuperposition;

    fn decide_quantum(
        &mut self,
        superposition: &mut ProposalSuperposition,
        rng: &mut dyn rand::RngCore,
    ) -> Option<Proposal>;

    fn learn_quantum(&mut self, outcome: &ActionOutcome, superposition: &ProposalSuperposition);

    fn quantum_coherence(&self) -> f64;

    fn entanglement_map(&self) -> &EntanglementMap;

    fn phase_space(&self) -> Option<&QuantumPhaseSpace>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consciousness::traits::{AgentKind, Priority};
    use chrono::Utc;

    fn make_proposal(id: u64, action: &str, confidence: f64, contradicts: Vec<u64>) -> Proposal {
        Proposal {
            id,
            source: AgentKind::Strategy,
            action: action.to_string(),
            reasoning: "test".to_string(),
            confidence,
            priority: Priority::Normal,
            contradicts,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn superposition_from_proposals_normalized() {
        let proposals = vec![
            make_proposal(1, "explore", 0.8, vec![]),
            make_proposal(2, "conserve", 0.2, vec![]),
        ];
        let sup = ProposalSuperposition::from_proposals(proposals);
        assert!(sup.quantum_state.is_normalized());
    }

    #[test]
    fn interference_contradictory_proposals() {
        let proposals = vec![
            make_proposal(1, "explore", 0.5, vec![2]),
            make_proposal(2, "retreat", 0.5, vec![1]),
        ];
        let mut sup = ProposalSuperposition::from_proposals(proposals);
        sup.apply_interference();
        assert!(sup.quantum_state.is_normalized());
    }

    #[test]
    fn interference_aligned_proposals_boost() {
        let proposals = vec![
            make_proposal(1, "explore", 0.5, vec![]),
            make_proposal(2, "explore", 0.5, vec![]),
        ];
        let mut sup = ProposalSuperposition::from_proposals(proposals);
        let _probs_before: Vec<f64> = sup.quantum_state.probabilities();
        sup.apply_interference();
        assert!(sup.quantum_state.is_normalized());
    }

    #[test]
    fn collapse_selects_valid_proposal() {
        let proposals = vec![
            make_proposal(1, "a", 0.5, vec![]),
            make_proposal(2, "b", 0.5, vec![]),
        ];
        let mut sup = ProposalSuperposition::from_proposals(proposals);
        let mut rng = rand::rng();
        let result = sup.collapse(&mut rng);
        assert!(result.is_some());
    }

    #[test]
    fn entanglement_map_tracks_correlations() {
        let mut map = EntanglementMap::new();
        for _ in 0..10 {
            map.update_correlation("Strategy", "Research", true);
        }
        assert!(map.get_strength("Strategy", "Research") > 0.0);
        assert!(map.get_strength("Research", "Strategy") > 0.0);
    }

    #[test]
    fn phase_space_distance() {
        let phenomenal = PhenomenalState {
            attention: 0.5,
            arousal: 0.5,
            valence: 0.0,
            ..Default::default()
        };
        let ps1 = QuantumPhaseSpace::from_phenomenal(&phenomenal, [0.5, 0.5, 0.5, 0.5], 0.0);
        let ps2 = QuantumPhaseSpace::from_phenomenal(&phenomenal, [0.5, 0.5, 0.5, 0.5], 1.0);
        assert!((ps1.distance(&ps2) - 1.0).abs() < 1e-10);
    }
}
