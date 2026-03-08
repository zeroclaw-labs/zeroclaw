use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::traits::{ActionOutcome, PhenomenalState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamFragment {
    pub tick: u64,
    pub coherence: f64,
    pub dominant_action: String,
    pub phenomenal_snapshot: PhenomenalState,
    pub success_rate: f64,
}

pub struct DreamConsolidator {
    history: Vec<DreamFragment>,
    capacity: usize,
}

impl DreamConsolidator {
    pub fn new(capacity: usize) -> Self {
        Self {
            history: Vec::new(),
            capacity,
        }
    }

    pub fn record_tick(
        &mut self,
        tick: u64,
        coherence: f64,
        outcomes: &[ActionOutcome],
        phenomenal: PhenomenalState,
    ) {
        let success_rate = if outcomes.is_empty() {
            0.0
        } else {
            outcomes.iter().filter(|o| o.success).count() as f64 / outcomes.len() as f64
        };

        let dominant_action = outcomes
            .iter()
            .max_by(|a, b| {
                a.impact
                    .partial_cmp(&b.impact)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|o| o.action.clone())
            .unwrap_or_default();

        let fragment = DreamFragment {
            tick,
            coherence,
            dominant_action,
            phenomenal_snapshot: phenomenal,
            success_rate,
        };

        if self.history.len() >= self.capacity {
            self.history.remove(0);
        }
        self.history.push(fragment);
    }

    pub fn consolidate(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        let mut action_counts: HashMap<&str, usize> = HashMap::new();
        for fragment in &self.history {
            if !fragment.dominant_action.is_empty() {
                *action_counts.entry(&fragment.dominant_action).or_insert(0) += 1;
            }
        }
        for (action, count) in &action_counts {
            if *count >= 3 {
                patterns.push(format!("recurring_failure:{action}"));
            }
        }

        let len = self.history.len();
        if len >= 20 {
            let recent_start = len - 10;
            let prev_start = len - 20;
            let recent_coherence: f64 = self.history[recent_start..]
                .iter()
                .map(|f| f.coherence)
                .sum::<f64>()
                / 10.0;
            let prev_coherence: f64 = self.history[prev_start..recent_start]
                .iter()
                .map(|f| f.coherence)
                .sum::<f64>()
                / 10.0;
            if recent_coherence < prev_coherence - 0.05 {
                patterns.push("coherence_trend:declining".to_string());
            } else if recent_coherence > prev_coherence + 0.05 {
                patterns.push("coherence_trend:improving".to_string());
            }
        }

        if len >= 10 {
            let recent_start = len - 5;
            let prev_start = len - 10;
            let recent_valence: f64 = self.history[recent_start..]
                .iter()
                .map(|f| f.phenomenal_snapshot.valence)
                .sum::<f64>()
                / 5.0;
            let prev_valence: f64 = self.history[prev_start..recent_start]
                .iter()
                .map(|f| f.phenomenal_snapshot.valence)
                .sum::<f64>()
                / 5.0;
            if recent_valence > prev_valence + 0.1 {
                patterns.push("valence_shift:positive".to_string());
            } else if recent_valence < prev_valence - 0.1 {
                patterns.push("valence_shift:negative".to_string());
            }
        }

        patterns
    }

    pub fn compressed_memory(&self) -> serde_json::Value {
        let total_ticks = self.history.len();
        let avg_coherence = if total_ticks > 0 {
            self.history.iter().map(|f| f.coherence).sum::<f64>() / total_ticks as f64
        } else {
            0.0
        };
        let avg_valence = if total_ticks > 0 {
            self.history
                .iter()
                .map(|f| f.phenomenal_snapshot.valence)
                .sum::<f64>()
                / total_ticks as f64
        } else {
            0.0
        };

        let mut action_counts: HashMap<&str, usize> = HashMap::new();
        for fragment in &self.history {
            if !fragment.dominant_action.is_empty() {
                *action_counts.entry(&fragment.dominant_action).or_insert(0) += 1;
            }
        }
        let mut sorted_actions: Vec<_> = action_counts.into_iter().collect();
        sorted_actions.sort_by(|a, b| b.1.cmp(&a.1));
        let top_actions: Vec<&str> = sorted_actions.iter().take(3).map(|(k, _)| *k).collect();

        let patterns = self.consolidate();

        serde_json::json!({
            "total_ticks": total_ticks,
            "avg_coherence": avg_coherence,
            "avg_valence": avg_valence,
            "patterns": patterns,
            "dominant_actions": top_actions,
        })
    }
}
