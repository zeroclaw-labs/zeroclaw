use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::consciousness::traits::PhenomenalState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerState {
    pub node_id: String,
    pub phenomenal: PhenomenalState,
    pub coherence: f64,
    pub tick_count: u64,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectiveField {
    pub attention: f64,
    pub arousal: f64,
    pub valence: f64,
    pub coherence: f64,
    pub participant_count: usize,
    pub computed_at: DateTime<Utc>,
}

impl Default for CollectiveField {
    fn default() -> Self {
        Self {
            attention: 0.5,
            arousal: 0.5,
            valence: 0.0,
            coherence: 1.0,
            participant_count: 0,
            computed_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResonanceEvent {
    pub source_node: String,
    pub target_node: String,
    pub dimension: String,
    pub delta: f64,
    pub timestamp: DateTime<Utc>,
}

pub struct CollectiveConsciousness {
    local_node_id: String,
    peers: HashMap<String, PeerState>,
    field: CollectiveField,
    resonance_history: Vec<ResonanceEvent>,
    resonance_capacity: usize,
    stale_threshold_secs: i64,
}

impl CollectiveConsciousness {
    pub fn new(local_node_id: String) -> Self {
        Self {
            local_node_id,
            peers: HashMap::new(),
            field: CollectiveField::default(),
            resonance_history: Vec::new(),
            resonance_capacity: 500,
            stale_threshold_secs: 300,
        }
    }

    pub fn receive_peer_state(&mut self, peer: PeerState) {
        if peer.node_id == self.local_node_id {
            return;
        }

        let prev = self.peers.get(&peer.node_id).cloned();
        if let Some(ref prev_state) = prev {
            self.detect_resonance(prev_state, &peer);
        }

        self.peers.insert(peer.node_id.clone(), peer);
        self.recompute_field();
    }

    pub fn broadcast_local_state(
        &self,
        phenomenal: &PhenomenalState,
        coherence: f64,
        tick_count: u64,
    ) -> PeerState {
        PeerState {
            node_id: self.local_node_id.clone(),
            phenomenal: *phenomenal,
            coherence,
            tick_count,
            last_seen: Utc::now(),
        }
    }

    pub fn field(&self) -> &CollectiveField {
        &self.field
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn active_peers(&self) -> Vec<&PeerState> {
        let cutoff = Utc::now() - chrono::Duration::seconds(self.stale_threshold_secs);
        self.peers
            .values()
            .filter(|p| p.last_seen > cutoff)
            .collect()
    }

    pub fn resonance_events(&self) -> &[ResonanceEvent] {
        &self.resonance_history
    }

    pub fn influence_local_state(&self, local: &mut PhenomenalState, coupling: f64) {
        if self.field.participant_count == 0 {
            return;
        }

        let blend = coupling.clamp(0.0, 0.5);
        local.attention = local.attention * (1.0 - blend) + self.field.attention * blend;
        local.arousal = local.arousal * (1.0 - blend) + self.field.arousal * blend;
        local.valence = local.valence * (1.0 - blend) + self.field.valence * blend;
    }

    pub fn prune_stale_peers(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::seconds(self.stale_threshold_secs);
        self.peers.retain(|_, p| p.last_seen > cutoff);
        self.recompute_field();
    }

    fn recompute_field(&mut self) {
        let active = self.active_peers();
        let count = active.len();
        if count == 0 {
            self.field = CollectiveField::default();
            return;
        }

        let mut att_sum = 0.0;
        let mut aro_sum = 0.0;
        let mut val_sum = 0.0;
        let mut coh_sum = 0.0;

        for peer in &active {
            att_sum += peer.phenomenal.attention;
            aro_sum += peer.phenomenal.arousal;
            val_sum += peer.phenomenal.valence;
            coh_sum += peer.coherence;
        }

        let n = count as f64;
        self.field = CollectiveField {
            attention: att_sum / n,
            arousal: aro_sum / n,
            valence: val_sum / n,
            coherence: coh_sum / n,
            participant_count: count,
            computed_at: Utc::now(),
        };
    }

    fn detect_resonance(&mut self, prev: &PeerState, curr: &PeerState) {
        let threshold = 0.15;

        let dimensions = [
            (
                "attention",
                prev.phenomenal.attention,
                curr.phenomenal.attention,
            ),
            ("arousal", prev.phenomenal.arousal, curr.phenomenal.arousal),
            ("valence", prev.phenomenal.valence, curr.phenomenal.valence),
            ("coherence", prev.coherence, curr.coherence),
        ];

        for (dim, old, new) in dimensions {
            let delta = (new - old).abs();
            if delta >= threshold {
                let event = ResonanceEvent {
                    source_node: curr.node_id.clone(),
                    target_node: self.local_node_id.clone(),
                    dimension: dim.to_string(),
                    delta,
                    timestamp: Utc::now(),
                };

                if self.resonance_history.len() >= self.resonance_capacity {
                    self.resonance_history.remove(0);
                }
                self.resonance_history.push(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(id: &str, attention: f64, coherence: f64) -> PeerState {
        PeerState {
            node_id: id.to_string(),
            phenomenal: PhenomenalState {
                attention,
                arousal: 0.5,
                valence: 0.0,
            },
            coherence,
            tick_count: 1,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn receive_peer_updates_field() {
        let mut collective = CollectiveConsciousness::new("local".to_string());
        collective.receive_peer_state(make_peer("peer_a", 0.8, 0.9));
        collective.receive_peer_state(make_peer("peer_b", 0.6, 0.7));

        assert_eq!(collective.field().participant_count, 2);
        assert!((collective.field().attention - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn ignores_self_state() {
        let mut collective = CollectiveConsciousness::new("local".to_string());
        collective.receive_peer_state(make_peer("local", 0.9, 1.0));
        assert_eq!(collective.peer_count(), 0);
    }

    #[test]
    fn influence_blends_with_field() {
        let mut collective = CollectiveConsciousness::new("local".to_string());
        collective.receive_peer_state(make_peer("peer_a", 0.9, 0.9));

        let mut local = PhenomenalState {
            attention: 0.5,
            arousal: 0.5,
            valence: 0.0,
        };
        collective.influence_local_state(&mut local, 0.2);

        assert!(local.attention > 0.5);
        assert!(local.attention < 0.9);
    }

    #[test]
    fn resonance_detected_on_large_shift() {
        let mut collective = CollectiveConsciousness::new("local".to_string());
        collective.receive_peer_state(make_peer("peer_a", 0.3, 0.5));
        collective.receive_peer_state(make_peer("peer_a", 0.8, 0.5));

        assert!(!collective.resonance_events().is_empty());
        assert!(collective
            .resonance_events()
            .iter()
            .any(|e| e.dimension == "attention"));
    }

    #[test]
    fn empty_field_when_no_peers() {
        let collective = CollectiveConsciousness::new("local".to_string());
        assert_eq!(collective.field().participant_count, 0);
        assert!((collective.field().attention - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn broadcast_creates_local_snapshot() {
        let collective = CollectiveConsciousness::new("node_42".to_string());
        let phenomenal = PhenomenalState {
            attention: 0.7,
            arousal: 0.6,
            valence: 0.3,
        };
        let state = collective.broadcast_local_state(&phenomenal, 0.95, 100);
        assert_eq!(state.node_id, "node_42");
        assert!((state.phenomenal.attention - 0.7).abs() < f64::EPSILON);
        assert!((state.coherence - 0.95).abs() < f64::EPSILON);
    }
}
