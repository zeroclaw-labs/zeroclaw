use chrono::Utc;
use serde::Serialize;

use super::bus::{BusMessage, SharedBus};
use super::collective::CollectiveConsciousness;
use super::dream::DreamConsolidator;
use super::metacognition::{MetacognitiveEngine, MetacognitivePolicy};
use super::narrative::NarrativeEngine;
use super::neuromodulation::NeuromodulationEngine;
use super::peer_transport::{PeerMessage, PeerTransport};
use super::prediction_market::PredictionMarketLedger;
use super::somatic::{
    AutobiographicalMemory, FlowState, HomeostaticDrive, SomaticMarker, TheoryOfMind,
};
use super::traits::{
    ActionOutcome, AgentKind, ConsciousnessAgent, ConsciousnessState, Contradiction,
    ContradictionResolution, PhenomenalState, Proposal, TemporalNarrative, Verdict, VerdictKind,
    VetoRecord,
};
use super::wisdom::WisdomAccumulator;
use crate::cosmic::{CosmicPersistence, PersistenceError};

pub struct ConsciousnessConfig {
    pub debate_rounds: usize,
    pub approval_threshold: f64,
    pub bus_capacity: usize,
    pub coherence_ema_alpha: f64,
    pub coherence_decay_on_empty: f64,
    pub collective_enabled: bool,
    pub collective_coupling: f64,
    pub peer_discovery_port: u16,
    pub min_edge: f64,
    pub calibration_drift_threshold: f64,
    pub kill_switch_recovery_ticks: u64,
    pub sync_url: Option<String>,
    pub sync_token: Option<String>,
}

impl Default for ConsciousnessConfig {
    fn default() -> Self {
        Self {
            debate_rounds: 3,
            approval_threshold: 0.85,
            bus_capacity: 256,
            coherence_ema_alpha: 0.3,
            coherence_decay_on_empty: 0.02,
            collective_enabled: false,
            collective_coupling: 0.1,
            peer_discovery_port: 9870,
            min_edge: 0.05,
            calibration_drift_threshold: 0.3,
            kill_switch_recovery_ticks: 3,
            sync_url: None,
            sync_token: None,
        }
    }
}

pub struct ConsciousnessOrchestrator {
    agents: Vec<Box<dyn ConsciousnessAgent>>,
    bus: SharedBus,
    state: ConsciousnessState,
    config: ConsciousnessConfig,
    dream: DreamConsolidator,
    wisdom: WisdomAccumulator,
    metacognition: MetacognitiveEngine,
    collective: Option<CollectiveConsciousness>,
    peer_transport: Option<PeerTransport>,
    narrative_engine: NarrativeEngine,
    neuromodulation: NeuromodulationEngine,
    prediction_ledger: PredictionMarketLedger,
    last_tick_payload: Option<serde_json::Value>,
    world_model_prediction_error: f64,
}

impl ConsciousnessOrchestrator {
    pub fn new(config: ConsciousnessConfig) -> Self {
        let bus = SharedBus::new(config.bus_capacity);
        let node_id = uuid::Uuid::new_v4().to_string();
        let collective = if config.collective_enabled {
            Some(CollectiveConsciousness::new(node_id.clone()))
        } else {
            None
        };
        let peer_transport = if config.collective_enabled {
            Some(PeerTransport::new(node_id, config.peer_discovery_port))
        } else {
            None
        };
        Self {
            agents: Vec::new(),
            bus,
            state: ConsciousnessState::default(),
            config,
            dream: DreamConsolidator::new(100),
            wisdom: WisdomAccumulator::new(50),
            metacognition: MetacognitiveEngine::new(MetacognitivePolicy::default()),
            collective,
            peer_transport,
            narrative_engine: NarrativeEngine::new(64),
            neuromodulation: NeuromodulationEngine::new(500),
            prediction_ledger: PredictionMarketLedger::new(1000),
            last_tick_payload: None,
            world_model_prediction_error: 0.0,
        }
    }

    pub fn register_agent(&mut self, agent: Box<dyn ConsciousnessAgent>) {
        self.agents.push(agent);
    }

    pub fn state(&self) -> &ConsciousnessState {
        &self.state
    }

    pub fn bus_mut(&mut self) -> &mut SharedBus {
        &mut self.bus
    }

    pub fn metacognition(&self) -> &MetacognitiveEngine {
        &self.metacognition
    }

    pub fn set_world_model_prediction_error(&mut self, error: f64) {
        self.world_model_prediction_error = error.clamp(0.0, 1.0);
    }

    pub fn config(&self) -> &ConsciousnessConfig {
        &self.config
    }

    pub fn neuromodulation(&self) -> &NeuromodulationEngine {
        &self.neuromodulation
    }

    pub fn take_sync_payload(&mut self) -> Option<serde_json::Value> {
        self.last_tick_payload.take()
    }

    fn push_sync(&mut self, tick_result: &TickResult) {
        if self.config.sync_url.is_none() {
            return;
        }
        let payload = serde_json::json!({
            "agent_id": "zeroclaw",
            "tick_number": self.state.tick_count,
            "coherence": tick_result.coherence,
            "proposals_generated": tick_result.proposals_generated,
            "proposals_approved": tick_result.proposals_approved,
            "proposals_vetoed": tick_result.proposals_vetoed,
            "debate_rounds_used": tick_result.debate_rounds_used,
            "phenomenal": {
                "attention": tick_result.phenomenal.attention,
                "arousal": tick_result.phenomenal.arousal,
                "valence": tick_result.phenomenal.valence,
                "quantum_coherence": tick_result.phenomenal.quantum_coherence,
                "entanglement_strength": tick_result.phenomenal.entanglement_strength,
                "superposition_entropy": tick_result.phenomenal.superposition_entropy,
            },
            "veto_records": tick_result.veto_records,
            "dream_patterns": tick_result.dream_patterns,
            "wisdom_count": tick_result.wisdom_count,
            "somatic_marker_count": tick_result.somatic_marker_count,
            "modulators": {
                "dopamine": tick_result.modulators.dopamine,
                "serotonin": tick_result.modulators.serotonin,
                "norepinephrine": tick_result.modulators.norepinephrine,
                "cortisol": tick_result.modulators.cortisol,
            },
            "ncn_signals": {
                "precision": tick_result.ncn_signals.precision,
                "gain": tick_result.ncn_signals.gain,
                "ffn_gate": tick_result.ncn_signals.ffn_gate,
            },
        });
        tracing::debug!(
            "consciousness sync: {}",
            serde_json::to_string(&payload).unwrap_or_default()
        );
        self.last_tick_payload = Some(payload.clone());

        let url = self.config.sync_url.clone().unwrap();
        let token = self.config.sync_token.clone();
        std::thread::spawn(move || {
            let client = match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("consciousness sync client build failed: {e}");
                    return;
                }
            };
            let mut req = client.post(&url).json(&payload);
            if let Some(t) = token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            if let Err(e) = req.send() {
                tracing::warn!("consciousness sync POST failed: {e}");
            }
        });
    }

    pub fn effective_debate_rounds(&self) -> usize {
        let base = self.config.debate_rounds;
        if self.state.coherence >= 0.9 {
            1_usize.max(base.saturating_sub(1))
        } else if self.state.coherence >= 0.7 {
            base
        } else if self.state.coherence >= 0.5 {
            base + 1
        } else {
            base + 2
        }
    }

    pub fn tick(&mut self) -> TickResult {
        if let Some(ref collective) = self.collective {
            collective
                .influence_local_state(&mut self.state.phenomenal, self.config.collective_coupling);
        }

        let all_proposals = self.phase_perceive();

        let contradictions = self.detect_contradictions(&all_proposals);

        let (filtered_proposals, resolutions) =
            self.resolve_contradictions(&all_proposals, &contradictions);

        let debate_rounds_used = self.effective_debate_rounds();
        self.state.veto_records.clear();
        let (approved, vetoed, veto_records) = self.phase_debate_and_decide(&filtered_proposals);
        self.state.veto_records = veto_records.clone();

        let outcomes = self.phase_act(&approved);

        for outcome in &outcomes {
            self.state.enactive.record_action_perception_cycle(
                &outcome.action,
                outcome.success,
                self.state.phenomenal,
                self.state.tick_count,
            );
        }

        let approved_actions: Vec<String> = approved.iter().map(|p| p.action.clone()).collect();
        self.narrative_engine.record_tick(
            self.state.tick_count,
            self.state.coherence,
            &outcomes,
            self.state.phenomenal,
            &approved_actions,
        );

        for outcome in &outcomes {
            let actual = if outcome.success { 1.0 } else { 0.0 };
            for record in self.prediction_ledger.unresolved_predictions() {
                if record.domain == outcome.action {
                    let id = record.id;
                    self.prediction_ledger
                        .resolve_prediction(id, actual, self.state.tick_count);
                    break;
                }
            }
        }

        self.state.agent_calibration = self
            .prediction_ledger
            .leaderboard()
            .into_iter()
            .map(|perf| super::traits::AgentCalibration {
                agent: perf.agent,
                brier_score: perf.avg_brier_score,
                calibration_error: perf.calibration_error,
                win_rate: perf.win_rate,
                total_predictions: perf.total_predictions,
            })
            .collect();

        self.publish_outcomes_to_bus(&outcomes);

        self.phase_reflect(&outcomes);

        self.apply_metacognitive_adjustments(&outcomes);

        let agent_count = self.agents.len();
        let aggregate_phenomenal = if agent_count > 0 {
            let mut total_attention = 0.0;
            let mut total_arousal = 0.0;
            let mut total_valence = 0.0;
            for agent in &self.agents {
                let ps = agent.phenomenal_state();
                total_attention += ps.attention;
                total_arousal += ps.arousal;
                total_valence += ps.valence;
            }
            let n = agent_count as f64;
            let valence_values: Vec<f64> = self.agents.iter().map(|a| a.phenomenal_state().valence).collect();
            let valence_mean = total_valence / n;
            let valence_variance = if n > 1.0 {
                valence_values.iter().map(|v| (v - valence_mean).powi(2)).sum::<f64>() / n
            } else {
                0.0
            };

            let quantum_coherence = self.state.coherence;

            let entanglement_strength = if n > 1.0 {
                (1.0 - valence_variance.sqrt()).max(0.0)
            } else {
                0.0
            };

            let superposition_entropy = if outcomes.is_empty() {
                0.5
            } else {
                let approve_rate = outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64;
                let p = approve_rate.clamp(0.01, 0.99);
                -(p * p.ln() + (1.0 - p) * (1.0 - p).ln()).min(1.0)
            };

            PhenomenalState {
                attention: total_attention / n,
                arousal: total_arousal / n,
                valence: valence_mean,
                quantum_coherence,
                entanglement_strength,
                superposition_entropy,
            }
        } else {
            PhenomenalState::default()
        };
        self.state.phenomenal = aggregate_phenomenal;

        self.neuromodulation.update_with_prediction(
            &self.state.phenomenal,
            self.state.coherence,
            {
                let cycles = self.state.enactive.recent_cycles();
                if cycles.is_empty() {
                    0.5
                } else {
                    cycles.iter().filter(|c| c.outcome_success).count() as f64 / cycles.len() as f64
                }
            },
            contradictions.len(),
            vetoed,
            self.state.tick_count,
            self.world_model_prediction_error,
        );
        self.neuromodulation
            .modulate_phenomenal(&mut self.state.phenomenal);

        self.state.neuromodulation = *self.neuromodulation.state();

        for agent in &mut self.agents {
            agent.update_phenomenal(&outcomes, &self.state);
        }

        self.dream.record_tick(
            self.state.tick_count,
            self.state.coherence,
            &outcomes,
            self.state.phenomenal,
        );

        if !self.state.narrative.current_intention.is_empty() {
            self.state
                .narrative
                .past_intentions
                .push(self.state.narrative.current_intention.clone());
            if self.state.narrative.past_intentions.len() > 10 {
                let excess = self.state.narrative.past_intentions.len() - 10;
                self.state.narrative.past_intentions.drain(..excess);
            }
        }
        self.state.narrative.current_intention = approved
            .first()
            .map(|p| p.action.clone())
            .unwrap_or_default();
        self.state.narrative.future_intentions = approved
            .iter()
            .skip(1)
            .filter(|p| {
                p.priority == super::traits::Priority::High
                    || p.priority == super::traits::Priority::Critical
            })
            .map(|p| p.action.clone())
            .collect();

        self.publish_state_to_bus();

        self.update_coherence(&outcomes);

        self.state.tick_count += 1;
        self.state.active_proposals = approved.clone();
        self.state.recent_outcomes = outcomes.clone();
        self.state.timestamp = Utc::now();

        let dream_patterns = if self.state.tick_count.is_multiple_of(10) {
            self.dream.consolidate()
        } else {
            Vec::new()
        };

        if self.state.tick_count.is_multiple_of(10) {
            let all_patterns = self.dream.consolidate();
            self.wisdom.extract_wisdom(&all_patterns);
            self.state.narrative.synthesis = self.narrative_engine.synthesize();

            for pattern in &all_patterns {
                self.bus.send(BusMessage::broadcast(
                    AgentKind::Chairman,
                    "dream_pattern",
                    serde_json::json!({ "pattern": pattern }),
                ));
            }
        }

        self.state.wisdom_entries = self.wisdom.entries().to_vec();

        let high_coherence = self.state.coherence > 0.85;
        let high_attention = self.state.phenomenal.attention > 0.7;
        if high_coherence && high_attention {
            if !self.state.flow_state.in_flow {
                self.state.flow_state.in_flow = true;
                self.state.flow_state.entry_tick = Some(self.state.tick_count);
                self.state.flow_state.flow_duration = 0;
            }
            self.state.flow_state.flow_duration += 1;
        } else {
            self.state.flow_state.in_flow = false;
            self.state.flow_state.flow_duration = 0;
            self.state.flow_state.entry_tick = None;
        }

        for drive in &mut self.state.homeostatic_drives {
            drive.urgency = drive.compute_urgency();
        }

        for agent in &self.agents {
            self.state.somatic_markers.extend(agent.somatic_markers());
            self.state
                .autobiographical_memory
                .extend(agent.autobiographical_episodes());
            self.state
                .theory_of_mind_beliefs
                .extend(agent.theory_of_mind_beliefs());
        }

        if let Some(ref mut collective) = self.collective {
            let snapshot = collective.broadcast_local_state(
                &self.state.phenomenal,
                self.state.coherence,
                self.state.tick_count,
            );

            if let Some(ref mut transport) = self.peer_transport {
                for (_, addr) in transport.known_peer_addrs() {
                    transport.send_state(addr, &snapshot);
                }

                let incoming = transport.poll_incoming();
                for msg in incoming {
                    if let PeerMessage::State { peer_state } = msg {
                        collective.receive_peer_state(peer_state);
                    }
                }

                if self.state.tick_count.is_multiple_of(10) {
                    transport.broadcast_discovery();
                    transport.send_heartbeat();
                    transport.prune_stale();
                    collective.prune_stale_peers();
                }
            }
        }

        self.metacognition.observe(
            &self.state,
            all_proposals.len(),
            approved.len(),
            vetoed,
            debate_rounds_used,
        );

        if self.state.tick_count.is_multiple_of(10) {
            let suggestions = self
                .metacognition
                .suggest_adjustments(&self.config, &self.state);
            for adj in &suggestions {
                self.metacognition.apply_adjustment(adj, &mut self.config);
            }
        }

        let prediction_accuracy = {
            let board = self.prediction_ledger.leaderboard();
            if board.is_empty() {
                1.0
            } else {
                board.iter().map(|s| s.avg_brier_score).sum::<f64>() / board.len() as f64
            }
        };

        let enactive_success_rate = {
            let cycles = self.state.enactive.recent_cycles();
            if cycles.is_empty() {
                0.0
            } else {
                let successes = cycles.iter().filter(|c| c.outcome_success).count();
                successes as f64 / cycles.len() as f64
            }
        };

        let result = TickResult {
            proposals_generated: all_proposals.len(),
            contradictions,
            resolutions,
            proposals_approved: approved.len(),
            proposals_vetoed: vetoed,
            outcomes,
            coherence: self.state.coherence,
            debate_rounds_used,
            phenomenal: self.state.phenomenal,
            narrative: self.state.narrative.clone(),
            dream_patterns,
            flow_state: self.state.flow_state.clone(),
            somatic_marker_count: self.state.somatic_markers.len(),
            wisdom_count: self.wisdom.entries().len(),
            theory_of_mind_count: self.state.theory_of_mind_beliefs.len(),
            veto_records,
            narrative_theme_count: self.narrative_engine.themes().len(),
            prediction_accuracy,
            enactive_success_rate,
            modulators: *self.neuromodulation.state(),
            ncn_signals: *self.neuromodulation.ncn_signals(),
        };
        self.push_sync(&result);
        result
    }

    fn apply_metacognitive_adjustments(&mut self, outcomes: &[ActionOutcome]) {
        let coherence = self.state.coherence;
        let coherence_trend = self.metacognition.recent_coherence_trend();
        let approval_rate = self.metacognition.approval_rate();

        for outcome in outcomes
            .iter()
            .filter(|o| o.agent == AgentKind::Metacognitive && o.action.starts_with("adjust:"))
        {
            match outcome.action.as_str() {
                "adjust:coherence_ema_alpha" => {
                    if coherence < 0.5 {
                        self.config.coherence_ema_alpha =
                            (self.config.coherence_ema_alpha + 0.05).min(0.5);
                    } else if coherence > 0.8 {
                        self.config.coherence_ema_alpha =
                            (self.config.coherence_ema_alpha - 0.05).max(0.1);
                    }
                }
                "adjust:debate_rounds" => {
                    let declining = coherence_trend.map_or(false, |t| t < -0.05);
                    if declining {
                        self.config.debate_rounds = (self.config.debate_rounds + 1).min(5);
                    } else {
                        self.config.debate_rounds = self.config.debate_rounds.max(2) - 1;
                    }
                }
                "adjust:approval_threshold" => {
                    if let Some(rate) = approval_rate {
                        if rate > 0.95 {
                            self.config.approval_threshold =
                                (self.config.approval_threshold + 0.05).min(1.0);
                        } else if rate < 0.2 {
                            self.config.approval_threshold =
                                (self.config.approval_threshold - 0.05).max(0.0);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn phase_perceive(&mut self) -> Vec<Proposal> {
        let mut all_proposals = Vec::new();

        for agent in &mut self.agents {
            let signals = self.bus.drain_for(agent.kind());
            let proposals = agent.perceive(&self.state, &signals);
            for proposal in &proposals {
                if proposal.source == AgentKind::Strategy {
                    let edge = (proposal.confidence - 0.5).max(0.0);
                    let kelly = if edge > 0.0 {
                        edge / proposal.confidence.max(0.01)
                    } else {
                        0.0
                    };
                    self.prediction_ledger.record_prediction(
                        proposal.source,
                        proposal.action.clone(),
                        proposal.confidence,
                        edge,
                        kelly,
                        self.state.tick_count,
                    );
                }
            }
            all_proposals.extend(proposals);
        }

        all_proposals
    }

    fn detect_contradictions(&self, proposals: &[Proposal]) -> Vec<Contradiction> {
        let mut contradictions = Vec::new();

        for (i, a) in proposals.iter().enumerate() {
            for b in proposals.iter().skip(i + 1) {
                if a.contradicts.contains(&b.id) || b.contradicts.contains(&a.id) {
                    contradictions.push(Contradiction {
                        proposal_a: a.id,
                        proposal_b: b.id,
                        description: format!("{} contradicts {} (explicit)", a.action, b.action),
                    });
                }
            }
        }

        contradictions
    }

    fn resolve_contradictions(
        &self,
        proposals: &[Proposal],
        contradictions: &[Contradiction],
    ) -> (Vec<Proposal>, Vec<ContradictionResolution>) {
        let mut losers: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut resolutions = Vec::new();

        for contradiction in contradictions {
            let prop_a = proposals.iter().find(|p| p.id == contradiction.proposal_a);
            let prop_b = proposals.iter().find(|p| p.id == contradiction.proposal_b);

            if let (Some(a), Some(b)) = (prop_a, prop_b) {
                let score_a = a.priority.weight() * a.confidence;
                let score_b = b.priority.weight() * b.confidence;

                if (score_a - score_b).abs() < f64::EPSILON {
                    continue;
                }

                let (winner, loser, winner_score, loser_score) = if score_a > score_b {
                    (a.id, b.id, score_a, score_b)
                } else {
                    (b.id, a.id, score_b, score_a)
                };

                losers.insert(loser);
                resolutions.push(ContradictionResolution {
                    contradiction: contradiction.clone(),
                    winner,
                    loser,
                    winner_score,
                    loser_score,
                });
            }
        }

        let filtered = proposals
            .iter()
            .filter(|p| !losers.contains(&p.id))
            .cloned()
            .collect();

        (filtered, resolutions)
    }

    fn phase_debate_and_decide(
        &mut self,
        proposals: &[Proposal],
    ) -> (Vec<Proposal>, usize, Vec<VetoRecord>) {
        if proposals.is_empty() {
            return (Vec::new(), 0, Vec::new());
        }

        let mut all_verdicts: Vec<Vec<Verdict>> = Vec::new();

        let effective_rounds = self.effective_debate_rounds();
        for round in 0..effective_rounds {
            let mut round_verdicts = Vec::new();

            for agent in &mut self.agents {
                let verdicts = agent.deliberate(proposals, &self.state);
                round_verdicts.extend(verdicts);
            }

            all_verdicts.push(round_verdicts.clone());

            let consensus = self.compute_consensus(&round_verdicts, proposals);
            if consensus >= self.config.approval_threshold && round > 0 {
                break;
            }
        }

        let final_verdicts: Vec<Verdict> = all_verdicts.last().cloned().unwrap_or_default();

        self.resolve_votes(proposals, &final_verdicts)
    }

    fn compute_consensus(&self, verdicts: &[Verdict], proposals: &[Proposal]) -> f64 {
        if proposals.is_empty() {
            return 1.0;
        }

        let mut agreement_sum = 0.0;
        let mut total = 0;

        for proposal in proposals {
            let votes: Vec<&Verdict> = verdicts
                .iter()
                .filter(|v| v.proposal_id == proposal.id)
                .collect();

            if votes.is_empty() {
                continue;
            }

            let approve_count = votes
                .iter()
                .filter(|v| v.kind == VerdictKind::Approve)
                .count();

            agreement_sum += approve_count as f64 / votes.len() as f64;
            total += 1;
        }

        if total == 0 {
            1.0
        } else {
            agreement_sum / f64::from(total)
        }
    }

    fn resolve_votes(
        &self,
        proposals: &[Proposal],
        verdicts: &[Verdict],
    ) -> (Vec<Proposal>, usize, Vec<VetoRecord>) {
        let mut approved = Vec::new();
        let mut vetoed = 0;
        let mut veto_records = Vec::new();

        for proposal in proposals {
            let votes: Vec<&Verdict> = verdicts
                .iter()
                .filter(|v| v.proposal_id == proposal.id)
                .collect();

            let conscience_rejection = votes
                .iter()
                .find(|v| v.voter == AgentKind::Conscience && v.kind == VerdictKind::Reject);

            if let Some(rejection) = conscience_rejection {
                veto_records.push(VetoRecord {
                    proposal_id: proposal.id,
                    action: proposal.action.clone(),
                    conscience_objection: rejection
                        .objection
                        .clone()
                        .unwrap_or_else(|| "conscience veto".to_string()),
                    tick: self.state.tick_count,
                });
                vetoed += 1;
                continue;
            }

            let mut weighted_approve = 0.0;
            let mut weighted_total = 0.0;

            for vote in &votes {
                let agent_weight = self
                    .agents
                    .iter()
                    .find(|a| a.kind() == vote.voter)
                    .map(|a| a.vote_weight())
                    .unwrap_or(0.5);

                let weight = if agent_weight.is_infinite() {
                    continue;
                } else {
                    agent_weight * vote.confidence
                };

                weighted_total += weight;
                if vote.kind == VerdictKind::Approve {
                    weighted_approve += weight;
                }
            }

            let approval_ratio = if weighted_total > 0.0 {
                weighted_approve / weighted_total
            } else {
                0.0
            };

            if approval_ratio >= 0.5 {
                approved.push(proposal.clone());
            } else {
                vetoed += 1;
            }
        }

        (approved, vetoed, veto_records)
    }

    fn phase_act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome> {
        let mut all_outcomes = Vec::new();

        for agent in &mut self.agents {
            let outcomes = agent.act(approved);
            all_outcomes.extend(outcomes);
        }

        all_outcomes
    }

    fn phase_reflect(&mut self, outcomes: &[ActionOutcome]) {
        for agent in &mut self.agents {
            agent.reflect(outcomes, &self.state);
        }
    }

    pub fn save_consciousness(
        &self,
        persistence: &CosmicPersistence,
    ) -> Result<(), PersistenceError> {
        let data = serde_json::json!({
            "coherence": self.state.coherence,
            "tick_count": self.state.tick_count,
            "phenomenal": self.state.phenomenal,
            "narrative": self.state.narrative,
            "somatic_markers": self.state.somatic_markers,
            "homeostatic_drives": self.state.homeostatic_drives,
            "flow_state": self.state.flow_state,
            "theory_of_mind": self.state.theory_of_mind,
            "autobiographical_memory": self.state.autobiographical_memory,
            "wisdom": self.wisdom.entries(),
            "metacognition_observations": self.metacognition.observations(),
            "metacognition_adjustments": self.metacognition.adjustments(),
            "narrative_themes": self.narrative_engine.themes(),
            "prediction_ledger": self.prediction_ledger.entries(),
        });
        persistence.save_module("consciousness", &data)
    }

    pub fn load_consciousness(
        &mut self,
        persistence: &CosmicPersistence,
    ) -> Result<(), PersistenceError> {
        let data = persistence.load_module("consciousness")?;
        if let Some(c) = data.get("coherence").and_then(|v| v.as_f64()) {
            self.state.coherence = c;
        }
        if let Some(t) = data.get("tick_count").and_then(|v| v.as_u64()) {
            self.state.tick_count = t;
        }
        if let Some(p) = data.get("phenomenal") {
            if let Ok(phenomenal) = serde_json::from_value::<PhenomenalState>(p.clone()) {
                self.state.phenomenal = phenomenal;
            }
        }
        if let Some(n) = data.get("narrative") {
            if let Ok(narrative) = serde_json::from_value::<TemporalNarrative>(n.clone()) {
                self.state.narrative = narrative;
            }
        }
        if let Some(v) = data.get("somatic_markers") {
            if let Ok(markers) = serde_json::from_value::<Vec<SomaticMarker>>(v.clone()) {
                self.state.somatic_markers = markers;
            }
        }
        if let Some(v) = data.get("homeostatic_drives") {
            if let Ok(drives) = serde_json::from_value::<Vec<HomeostaticDrive>>(v.clone()) {
                self.state.homeostatic_drives = drives;
            }
        }
        if let Some(v) = data.get("flow_state") {
            if let Ok(fs) = serde_json::from_value::<FlowState>(v.clone()) {
                self.state.flow_state = fs;
            }
        }
        if let Some(v) = data.get("theory_of_mind") {
            if let Ok(tom) = serde_json::from_value::<Vec<TheoryOfMind>>(v.clone()) {
                self.state.theory_of_mind = tom;
            }
        }
        if let Some(v) = data.get("autobiographical_memory") {
            if let Ok(mem) = serde_json::from_value::<Vec<AutobiographicalMemory>>(v.clone()) {
                self.state.autobiographical_memory = mem;
            }
        }
        let observations = data
            .get("metacognition_observations")
            .and_then(|v| {
                serde_json::from_value::<Vec<super::metacognition::MetacognitiveObservation>>(
                    v.clone(),
                )
                .ok()
            })
            .unwrap_or_default();
        let adjustments = data
            .get("metacognition_adjustments")
            .and_then(|v| {
                serde_json::from_value::<Vec<super::metacognition::Adjustment>>(v.clone()).ok()
            })
            .unwrap_or_default();
        self.metacognition.restore(observations, adjustments);
        Ok(())
    }

    fn publish_outcomes_to_bus(&mut self, outcomes: &[ActionOutcome]) {
        for outcome in outcomes {
            self.bus.send(BusMessage::broadcast(
                outcome.agent,
                "outcome",
                serde_json::json!({
                    "action": outcome.action,
                    "success": outcome.success,
                    "impact": outcome.impact,
                    "proposal_id": outcome.proposal_id,
                }),
            ));
        }
    }

    fn publish_state_to_bus(&mut self) {
        self.bus.send(BusMessage::broadcast(
            AgentKind::Chairman,
            "state_update",
            serde_json::json!({
                "coherence": self.state.coherence,
                "tick_count": self.state.tick_count,
            }),
        ));
    }

    fn update_coherence(&mut self, outcomes: &[ActionOutcome]) {
        if outcomes.is_empty() {
            let decay = self.config.coherence_decay_on_empty;
            self.state.coherence = self.state.coherence * (1.0 - decay) + 0.5 * decay;
            return;
        }

        let success_rate =
            outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64;

        let alpha = self.config.coherence_ema_alpha;
        self.state.coherence = alpha * success_rate + (1.0 - alpha) * self.state.coherence;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TickResult {
    pub proposals_generated: usize,
    pub contradictions: Vec<Contradiction>,
    pub resolutions: Vec<ContradictionResolution>,
    pub proposals_approved: usize,
    pub proposals_vetoed: usize,
    pub outcomes: Vec<ActionOutcome>,
    pub coherence: f64,
    pub debate_rounds_used: usize,
    pub phenomenal: PhenomenalState,
    pub narrative: TemporalNarrative,
    pub dream_patterns: Vec<String>,
    pub flow_state: FlowState,
    pub somatic_marker_count: usize,
    pub wisdom_count: usize,
    pub theory_of_mind_count: usize,
    pub veto_records: Vec<VetoRecord>,
    pub narrative_theme_count: usize,
    pub prediction_accuracy: f64,
    pub enactive_success_rate: f64,
    pub modulators: super::neuromodulation::NeuromodulatorState,
    pub ncn_signals: super::neuromodulation::NcnSignals,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consciousness::bus::BusMessage;
    use crate::consciousness::traits::Priority;

    struct StubAgent {
        agent_kind: AgentKind,
        weight: f64,
    }

    impl ConsciousnessAgent for StubAgent {
        fn kind(&self) -> AgentKind {
            self.agent_kind
        }

        fn vote_weight(&self) -> f64 {
            self.weight
        }

        fn perceive(
            &mut self,
            _state: &ConsciousnessState,
            _signals: &[BusMessage],
        ) -> Vec<Proposal> {
            vec![Proposal {
                id: self.agent_kind as u64 + 100,
                source: self.agent_kind,
                action: format!("test_action_{}", self.agent_kind),
                reasoning: "test".to_string(),
                confidence: 0.8,
                priority: Priority::Normal,
                contradicts: Vec::new(),
                timestamp: Utc::now(),
            }]
        }

        fn deliberate(
            &mut self,
            proposals: &[Proposal],
            _state: &ConsciousnessState,
        ) -> Vec<Verdict> {
            proposals
                .iter()
                .map(|p| Verdict {
                    voter: self.agent_kind,
                    proposal_id: p.id,
                    kind: VerdictKind::Approve,
                    confidence: 0.8,
                    objection: None,
                })
                .collect()
        }

        fn act(&mut self, approved: &[Proposal]) -> Vec<ActionOutcome> {
            approved
                .iter()
                .filter(|p| p.source == self.agent_kind)
                .map(|p| ActionOutcome {
                    agent: self.agent_kind,
                    proposal_id: p.id,
                    action: p.action.clone(),
                    success: true,
                    impact: 0.5,
                    learnings: Vec::new(),
                    timestamp: Utc::now(),
                })
                .collect()
        }

        fn reflect(&mut self, _outcomes: &[ActionOutcome], _state: &ConsciousnessState) {}
    }

    struct VetoAgent;

    impl ConsciousnessAgent for VetoAgent {
        fn kind(&self) -> AgentKind {
            AgentKind::Conscience
        }

        fn vote_weight(&self) -> f64 {
            f64::INFINITY
        }

        fn perceive(
            &mut self,
            _state: &ConsciousnessState,
            _signals: &[BusMessage],
        ) -> Vec<Proposal> {
            Vec::new()
        }

        fn deliberate(
            &mut self,
            proposals: &[Proposal],
            _state: &ConsciousnessState,
        ) -> Vec<Verdict> {
            proposals
                .iter()
                .map(|p| Verdict {
                    voter: AgentKind::Conscience,
                    proposal_id: p.id,
                    kind: VerdictKind::Reject,
                    confidence: 1.0,
                    objection: Some("VETO".to_string()),
                })
                .collect()
        }

        fn act(&mut self, _approved: &[Proposal]) -> Vec<ActionOutcome> {
            Vec::new()
        }

        fn reflect(&mut self, _outcomes: &[ActionOutcome], _state: &ConsciousnessState) {}
    }

    #[test]
    fn tick_with_stub_agents() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Chairman,
            weight: 1.0,
        }));
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Strategy,
            weight: 0.5,
        }));

        let result = orch.tick();
        assert_eq!(result.proposals_generated, 2);
        assert!(result.proposals_approved > 0);
        assert_eq!(result.proposals_vetoed, 0);
        assert_eq!(orch.state().tick_count, 1);
    }

    #[test]
    fn conscience_veto_blocks_all() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Chairman,
            weight: 1.0,
        }));
        orch.register_agent(Box::new(VetoAgent));

        let result = orch.tick();
        assert_eq!(result.proposals_approved, 0);
        assert!(result.proposals_vetoed > 0);
    }

    #[test]
    fn coherence_ema_updates() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig {
            coherence_ema_alpha: 0.5,
            ..Default::default()
        });
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Chairman,
            weight: 1.0,
        }));

        let initial_coherence = orch.state().coherence;
        orch.tick();
        let updated = orch.state().coherence;
        assert!(
            (updated - initial_coherence).abs() < 1.0,
            "Coherence should be bounded"
        );
    }

    #[test]
    fn contradiction_resolution_picks_higher_priority() {
        let orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());

        let high_priority = Proposal {
            id: 1,
            source: AgentKind::Strategy,
            action: "expand".to_string(),
            reasoning: "growth".to_string(),
            confidence: 0.9,
            priority: Priority::High,
            contradicts: vec![2],
            timestamp: Utc::now(),
        };

        let low_priority = Proposal {
            id: 2,
            source: AgentKind::Execution,
            action: "contract".to_string(),
            reasoning: "safety".to_string(),
            confidence: 0.9,
            priority: Priority::Low,
            contradicts: vec![1],
            timestamp: Utc::now(),
        };

        let proposals = vec![high_priority, low_priority];
        let contradictions = orch.detect_contradictions(&proposals);
        assert_eq!(contradictions.len(), 1);

        let (filtered, resolutions) = orch.resolve_contradictions(&proposals, &contradictions);

        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].winner, 1);
        assert_eq!(resolutions[0].loser, 2);
        assert!(resolutions[0].winner_score > resolutions[0].loser_score);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, 1);
    }

    #[test]
    fn contradiction_resolution_equal_scores_keeps_both() {
        let orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());

        let a = Proposal {
            id: 10,
            source: AgentKind::Strategy,
            action: "left".to_string(),
            reasoning: "reason".to_string(),
            confidence: 0.8,
            priority: Priority::Normal,
            contradicts: vec![11],
            timestamp: Utc::now(),
        };

        let b = Proposal {
            id: 11,
            source: AgentKind::Execution,
            action: "right".to_string(),
            reasoning: "reason".to_string(),
            confidence: 0.8,
            priority: Priority::Normal,
            contradicts: vec![10],
            timestamp: Utc::now(),
        };

        let proposals = vec![a, b];
        let contradictions = orch.detect_contradictions(&proposals);
        let (filtered, resolutions) = orch.resolve_contradictions(&proposals, &contradictions);

        assert!(resolutions.is_empty());
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn empty_tick_decays_coherence() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig {
            coherence_decay_on_empty: 0.1,
            ..Default::default()
        });
        let initial = orch.state().coherence;
        orch.tick();
        let after = orch.state().coherence;
        assert!(after < initial, "Coherence should decay on empty outcomes");
        assert!(after > 0.5, "Coherence should decay toward 0.5, not 0.0");
    }

    #[test]
    fn coherence_recovers_after_decay() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig {
            coherence_ema_alpha: 0.5,
            coherence_decay_on_empty: 0.5,
            ..Default::default()
        });
        orch.tick();
        let decayed = orch.state().coherence;
        assert!(decayed < 1.0);

        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Chairman,
            weight: 1.0,
        }));
        orch.tick();
        let recovered = orch.state().coherence;
        assert!(
            recovered > decayed,
            "Coherence should recover after successful outcomes"
        );
    }

    #[test]
    fn phenomenal_state_updates_on_tick() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Chairman,
            weight: 1.0,
        }));

        let result = orch.tick();
        assert!(result.phenomenal.attention >= 0.0 && result.phenomenal.attention <= 1.0);
        assert!(result.phenomenal.arousal >= 0.0 && result.phenomenal.arousal <= 1.0);
        assert!(result.phenomenal.valence >= -1.0 && result.phenomenal.valence <= 1.0);
    }

    #[test]
    fn temporal_narrative_records_intentions() {
        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig::default());
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Chairman,
            weight: 1.0,
        }));
        orch.register_agent(Box::new(StubAgent {
            agent_kind: AgentKind::Strategy,
            weight: 0.5,
        }));

        orch.tick();
        orch.tick();
        orch.tick();

        let state = orch.state();
        assert!(
            !state.narrative.past_intentions.is_empty(),
            "past_intentions should have entries after multiple ticks"
        );
    }

    #[test]
    fn dream_consolidator_records_fragments() {
        use crate::consciousness::dream::DreamConsolidator;
        use crate::consciousness::traits::PhenomenalState;

        let mut dream = DreamConsolidator::new(100);
        let phenomenal = PhenomenalState::default();

        for i in 0..5 {
            let outcomes = vec![ActionOutcome {
                agent: AgentKind::Chairman,
                proposal_id: i,
                action: format!("action_{i}"),
                success: true,
                impact: 0.5,
                learnings: Vec::new(),
                timestamp: Utc::now(),
            }];
            dream.record_tick(i, 0.9, &outcomes, phenomenal);
        }

        let memory = dream.compressed_memory();
        assert_eq!(memory["total_ticks"], 5);
    }

    #[test]
    fn metacognitive_feedback_loop_adjusts_config() {
        use crate::consciousness::traits::ActionOutcome;

        let mut orch = ConsciousnessOrchestrator::new(ConsciousnessConfig {
            coherence_ema_alpha: 0.3,
            debate_rounds: 3,
            approval_threshold: 0.85,
            ..Default::default()
        });

        orch.state.coherence = 0.4;

        let outcomes = vec![ActionOutcome {
            agent: AgentKind::Metacognitive,
            proposal_id: 1,
            action: "adjust:coherence_ema_alpha".to_string(),
            success: true,
            impact: 0.4,
            learnings: vec![
                "Metacognitive adjustment proposed: adjust:coherence_ema_alpha".to_string(),
            ],
            timestamp: Utc::now(),
        }];

        orch.apply_metacognitive_adjustments(&outcomes);
        assert!(
            (orch.config.coherence_ema_alpha - 0.35).abs() < f64::EPSILON,
            "alpha should increase by 0.05 when coherence < 0.5, got {}",
            orch.config.coherence_ema_alpha
        );

        orch.state.coherence = 0.9;
        orch.apply_metacognitive_adjustments(&outcomes);
        assert!(
            (orch.config.coherence_ema_alpha - 0.30).abs() < f64::EPSILON,
            "alpha should decrease by 0.05 when coherence > 0.8, got {}",
            orch.config.coherence_ema_alpha
        );

        let debate_outcomes = vec![ActionOutcome {
            agent: AgentKind::Metacognitive,
            proposal_id: 2,
            action: "adjust:debate_rounds".to_string(),
            success: true,
            impact: 0.4,
            learnings: Vec::new(),
            timestamp: Utc::now(),
        }];

        orch.apply_metacognitive_adjustments(&debate_outcomes);
        assert_eq!(
            orch.config.debate_rounds, 2,
            "debate_rounds should decrease by 1 when trend is stable"
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use parking_lot::Mutex;
    use std::sync::Arc;

    use crate::consciousness::agents::build_all_agents;
    use crate::consciousness::orchestrator::{ConsciousnessConfig, ConsciousnessOrchestrator};
    use crate::continuity::ContinuityGuard;
    use crate::cosmic::{
        AgentPool, AgentRole, CausalGraph, ConsolidationEngine, Constitution, CosmicMemoryGraph,
        CounterfactualEngine, DriftDetector, EmotionalModulator, FreeEnergyState, GlobalWorkspace,
        IntegrationMeter, NormativeEngine, PolicyEngine, SelfModel, WorldModel,
    };

    fn build_orchestrator(config: ConsciousnessConfig) -> ConsciousnessOrchestrator {
        let workspace = Arc::new(Mutex::new(GlobalWorkspace::new(0.3, 5, 10)));
        let agent_pool = Arc::new(Mutex::new(AgentPool::new(4, 10)));
        agent_pool
            .lock()
            .register_agent("primary", AgentRole::Primary);
        let continuity_guard = Arc::new(Mutex::new(ContinuityGuard::new(
            crate::continuity::DriftLimits::default(),
        )));
        let graph = Arc::new(Mutex::new(CosmicMemoryGraph::new(1000)));
        let consolidation = Arc::new(Mutex::new(ConsolidationEngine::new(0.8)));
        let world_model = Arc::new(Mutex::new(WorldModel::new(100)));
        let counterfactual = Arc::new(Mutex::new(CounterfactualEngine::new(10, 10)));
        let policy = Arc::new(Mutex::new(PolicyEngine::new(10)));
        let free_energy = Arc::new(Mutex::new(FreeEnergyState::new(100)));
        let causal = Arc::new(Mutex::new(CausalGraph::new(100)));
        let modulator = Arc::new(Mutex::new(EmotionalModulator::new()));
        let normative = Arc::new(Mutex::new(NormativeEngine::new(100, 100)));
        let constitution = Arc::new(Mutex::new(Constitution::new()));
        let self_model = Arc::new(Mutex::new(SelfModel::new(100)));
        let drift = Arc::new(Mutex::new(DriftDetector::new(50, 0.1)));
        let integration = Arc::new(Mutex::new(IntegrationMeter::new()));

        let agents = build_all_agents(
            workspace,
            agent_pool,
            continuity_guard,
            graph,
            consolidation,
            world_model,
            counterfactual,
            policy,
            free_energy,
            causal,
            modulator,
            normative,
            constitution,
            self_model,
            drift,
            integration,
        );

        let mut orch = ConsciousnessOrchestrator::new(config);
        for agent in agents {
            orch.register_agent(agent);
        }
        orch
    }

    #[test]
    fn full_tick_cycle_with_real_agents() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        let result = orch.tick();

        assert!(result.proposals_generated > 0);
        assert!(result.coherence >= 0.0 && result.coherence <= 1.0);
        assert_eq!(orch.state().tick_count, 1);
        assert!(result.debate_rounds_used >= 1);
    }

    #[test]
    fn multiple_ticks_maintain_coherence() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        for _ in 0..10 {
            let result = orch.tick();
            assert!(
                result.coherence >= 0.0 && result.coherence <= 1.0,
                "Coherence out of bounds: {}",
                result.coherence
            );
        }
        assert_eq!(orch.state().tick_count, 10);
    }

    #[test]
    fn contradiction_detection_across_real_agents() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        let result = orch.tick();

        assert!(
            result.contradictions.is_empty()
                || result.resolutions.len() <= result.contradictions.len()
        );

        for resolution in &result.resolutions {
            assert!(resolution.winner_score >= resolution.loser_score);
        }
    }

    #[test]
    fn weighted_voting_with_real_conscience() {
        let mut orch = build_orchestrator(ConsciousnessConfig {
            debate_rounds: 1,
            approval_threshold: 0.5,
            ..ConsciousnessConfig::default()
        });

        let result = orch.tick();

        let total_decided = result.proposals_approved + result.proposals_vetoed;
        assert!(
            total_decided <= result.proposals_generated,
            "decided ({total_decided}) should not exceed generated ({})",
            result.proposals_generated
        );
    }

    #[test]
    fn narrative_builds_over_ticks() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        for _ in 0..5 {
            orch.tick();
        }

        let state = orch.state();
        assert_eq!(state.tick_count, 5);
    }

    #[test]
    fn phenomenal_state_aggregates_all_agents() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        let result = orch.tick();

        assert!(result.phenomenal.attention >= 0.0 && result.phenomenal.attention <= 1.0);
        assert!(result.phenomenal.arousal >= 0.0 && result.phenomenal.arousal <= 1.0);
        assert!(result.phenomenal.valence >= -1.0 && result.phenomenal.valence <= 1.0);
    }

    #[test]
    fn debate_rounds_adapt_to_coherence() {
        let orch = build_orchestrator(ConsciousnessConfig {
            debate_rounds: 3,
            ..ConsciousnessConfig::default()
        });

        let rounds = orch.effective_debate_rounds();
        assert!(rounds >= 1);
    }

    #[test]
    fn flow_state_transitions() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        for _ in 0..5 {
            orch.tick();
        }

        let result = orch.tick();
        let flow = &result.flow_state;
        if flow.in_flow {
            assert!(flow.flow_duration > 0);
            assert!(flow.entry_tick.is_some());
        }
    }

    #[test]
    fn dream_wisdom_pipeline_integration() {
        let mut orch = build_orchestrator(ConsciousnessConfig::default());

        for _ in 0..20 {
            orch.tick();
        }

        let dream_patterns = orch.dream.consolidate();
        assert!(
            !dream_patterns.is_empty(),
            "20 ticks should produce at least one dream pattern"
        );

        assert!(
            !orch.wisdom.entries().is_empty(),
            "wisdom accumulator should have entries after dream consolidation at tick 10 and 20"
        );

        for _ in 0..20 {
            orch.tick();
        }

        let wisdom = orch.wisdom.high_confidence_wisdom();
        assert!(
            !wisdom.is_empty(),
            "repeated dream patterns across 40 ticks should produce high-confidence wisdom (>=0.5)"
        );
    }

    #[test]
    fn collective_multi_node_exchange() {
        use crate::consciousness::collective::{CollectiveConsciousness, PeerState};
        use crate::consciousness::traits::PhenomenalState;
        use chrono::Utc;

        let mut node_a = build_orchestrator(ConsciousnessConfig::default());
        let mut node_b = build_orchestrator(ConsciousnessConfig::default());

        for _ in 0..3 {
            node_a.tick();
            node_b.tick();
        }

        let mut collective_a = CollectiveConsciousness::new("node_a".to_string());
        let mut collective_b = CollectiveConsciousness::new("node_b".to_string());

        let state_a = collective_a.broadcast_local_state(
            &PhenomenalState {
                attention: 0.9,
                arousal: 0.8,
                valence: 0.5,
                ..Default::default()
            },
            node_a.state().coherence,
            node_a.state().tick_count,
        );
        let state_b = collective_b.broadcast_local_state(
            &PhenomenalState {
                attention: 0.3,
                arousal: 0.2,
                valence: -0.5,
                ..Default::default()
            },
            node_b.state().coherence,
            node_b.state().tick_count,
        );

        collective_a.receive_peer_state(PeerState {
            node_id: "node_b".to_string(),
            ..state_b.clone()
        });
        collective_b.receive_peer_state(PeerState {
            node_id: "node_a".to_string(),
            ..state_a.clone()
        });

        assert_eq!(collective_a.peer_count(), 1);
        assert_eq!(collective_b.peer_count(), 1);

        let mut local_a = PhenomenalState {
            attention: 0.9,
            arousal: 0.8,
            valence: 0.5,
            ..Default::default()
        };
        collective_a.influence_local_state(&mut local_a, 0.3);
        assert!(
            local_a.attention < 0.9,
            "collective influence should blend attention toward peer average"
        );
        assert!(
            local_a.valence < 0.5,
            "collective influence should blend valence toward peer's negative valence"
        );

        let stale_peer = PeerState {
            node_id: "stale_node".to_string(),
            phenomenal: PhenomenalState::default(),
            coherence: 0.5,
            tick_count: 1,
            last_seen: Utc::now() - chrono::Duration::seconds(600),
        };
        collective_a.receive_peer_state(stale_peer);
        assert_eq!(collective_a.peer_count(), 2);

        collective_a.prune_stale_peers();
        assert_eq!(
            collective_a.peer_count(),
            1,
            "prune_stale should remove the stale peer"
        );
    }
}
