use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::traits::{CognitiveLayer, CognitiveLayerOutput, LayerDecision};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub id: String,
    pub context: String,
    pub choice: String,
    pub outcome: Option<f64>,
    pub confidence: f64,
    pub tick: u64,
    pub source_id: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecisionMemory {
    pub decisions: Vec<DecisionRecord>,
    pub context_index: HashMap<String, Vec<usize>>,
}

impl DecisionMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, id: &str, context: &str, choice: &str, confidence: f64, tick: u64) {
        let idx = self.decisions.len();
        self.decisions.push(DecisionRecord {
            id: id.to_string(),
            context: context.to_string(),
            choice: choice.to_string(),
            outcome: None,
            confidence: confidence.clamp(0.0, 1.0),
            tick,
            source_id: String::new(),
            timestamp: tick,
        });
        self.context_index
            .entry(context.to_string())
            .or_default()
            .push(idx);
    }

    pub fn set_outcome(&mut self, id: &str, outcome: f64) {
        if let Some(d) = self.decisions.iter_mut().find(|d| d.id == id) {
            d.outcome = Some(outcome);
        }
    }

    pub fn query_similar(&self, context: &str) -> Vec<&DecisionRecord> {
        self.context_index
            .get(context)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&i| self.decisions.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn outcome_stats(&self, context: &str) -> (f64, usize) {
        let similar = self.query_similar(context);
        let with_outcome: Vec<f64> = similar.iter().filter_map(|d| d.outcome).collect();
        if with_outcome.is_empty() {
            return (0.0, 0);
        }
        let avg = with_outcome.iter().sum::<f64>() / with_outcome.len() as f64;
        (avg, with_outcome.len())
    }
}

impl CognitiveLayer for DecisionMemory {
    fn name(&self) -> &'static str {
        "decision_memory"
    }

    fn process(&self, input: &str) -> CognitiveLayerOutput {
        let similar = self.query_similar(input);
        let (avg_outcome, count) = self.outcome_stats(input);
        CognitiveLayerOutput {
            layer: self.name().to_string(),
            decision: if count > 0 && avg_outcome > 0.7 {
                LayerDecision::Read
            } else if count > 0 {
                LayerDecision::Defer
            } else {
                LayerDecision::Write
            },
            reasoning_summary: format!("{count} prior decisions, avg outcome: {avg_outcome:.2}"),
            confidence: if count > 0 {
                similar.iter().map(|d| d.confidence).sum::<f64>() / similar.len() as f64
            } else {
                0.0
            },
            source_id: "decision_memory".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_query() {
        let mut dm = DecisionMemory::new();
        dm.record("d1", "deploy", "canary", 0.8, 1);
        dm.record("d2", "deploy", "blue_green", 0.6, 2);
        dm.set_outcome("d1", 0.9);
        let similar = dm.query_similar("deploy");
        assert_eq!(similar.len(), 2);
        let (avg, count) = dm.outcome_stats("deploy");
        assert_eq!(count, 1);
        assert!((avg - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn cognitive_layer_trait_decision() {
        let mut dm = DecisionMemory::new();
        assert_eq!(dm.name(), "decision_memory");
        let output_empty = dm.process("deploy");
        assert_eq!(output_empty.decision, LayerDecision::Write);
        dm.record("d1", "deploy", "canary", 0.8, 1);
        dm.set_outcome("d1", 0.9);
        let output = dm.process("deploy");
        assert_eq!(output.decision, LayerDecision::Read);
    }

    #[test]
    fn empty_context_returns_nothing() {
        let dm = DecisionMemory::new();
        assert!(dm.query_similar("unknown").is_empty());
        let (avg, count) = dm.outcome_stats("unknown");
        assert_eq!(count, 0);
        assert_eq!(avg, 0.0);
    }
}
