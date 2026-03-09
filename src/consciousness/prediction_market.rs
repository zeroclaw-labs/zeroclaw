use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::traits::AgentKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionRecord {
    pub id: u64,
    pub agent: AgentKind,
    pub domain: String,
    pub predicted_outcome: f64,
    pub actual_outcome: Option<f64>,
    pub edge: f64,
    pub kelly_fraction: f64,
    pub brier_score: Option<f64>,
    pub resolved: bool,
    pub tick_created: u64,
    pub tick_resolved: Option<u64>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyPerformance {
    pub agent: AgentKind,
    pub total_predictions: u64,
    pub resolved_predictions: u64,
    pub avg_brier_score: f64,
    pub avg_edge: f64,
    pub calibration_error: f64,
    pub win_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionMarketLedger {
    records: Vec<PredictionRecord>,
    capacity: usize,
    next_id: u64,
}

impl PredictionMarketLedger {
    pub fn new(capacity: usize) -> Self {
        Self {
            records: Vec::with_capacity(capacity.min(1024)),
            capacity,
            next_id: 0,
        }
    }

    pub fn record_prediction(
        &mut self,
        agent: AgentKind,
        domain: String,
        posterior: f64,
        edge: f64,
        kelly: f64,
        tick: u64,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        if self.records.len() >= self.capacity {
            self.records.remove(0);
        }

        self.records.push(PredictionRecord {
            id,
            agent,
            domain,
            predicted_outcome: posterior,
            actual_outcome: None,
            edge,
            kelly_fraction: kelly,
            brier_score: None,
            resolved: false,
            tick_created: tick,
            tick_resolved: None,
            timestamp: Utc::now(),
        });

        id
    }

    pub fn resolve_prediction(&mut self, id: u64, actual_outcome: f64, tick: u64) {
        if let Some(record) = self.records.iter_mut().find(|r| r.id == id && !r.resolved) {
            record.actual_outcome = Some(actual_outcome);
            record.brier_score = Some((record.predicted_outcome - actual_outcome).powi(2));
            record.resolved = true;
            record.tick_resolved = Some(tick);
        }
    }

    pub fn agent_performance(&self, agent: AgentKind) -> StrategyPerformance {
        let agent_records: Vec<&PredictionRecord> =
            self.records.iter().filter(|r| r.agent == agent).collect();
        let total_predictions = agent_records.len() as u64;

        let resolved: Vec<&&PredictionRecord> =
            agent_records.iter().filter(|r| r.resolved).collect();
        let resolved_predictions = resolved.len() as u64;

        let avg_brier_score = if resolved.is_empty() {
            1.0
        } else {
            resolved.iter().filter_map(|r| r.brier_score).sum::<f64>() / resolved.len() as f64
        };

        let avg_edge = if agent_records.is_empty() {
            0.0
        } else {
            agent_records.iter().map(|r| r.edge).sum::<f64>() / agent_records.len() as f64
        };

        let calibration_error = if resolved.is_empty() {
            1.0
        } else {
            let sum: f64 = resolved
                .iter()
                .map(|r| {
                    let actual = r.actual_outcome.unwrap_or(0.0);
                    (r.predicted_outcome - actual).abs()
                })
                .sum();
            sum / resolved.len() as f64
        };

        let win_rate = if resolved.is_empty() {
            0.0
        } else {
            let wins = resolved
                .iter()
                .filter(|r| {
                    r.edge > 0.0
                        && r.actual_outcome
                            .is_some_and(|a| (r.predicted_outcome - a).abs() < 0.5)
                })
                .count();
            wins as f64 / resolved.len() as f64
        };

        StrategyPerformance {
            agent,
            total_predictions,
            resolved_predictions,
            avg_brier_score,
            avg_edge,
            calibration_error,
            win_rate,
        }
    }

    pub fn leaderboard(&self) -> Vec<StrategyPerformance> {
        let mut agents: Vec<AgentKind> = self
            .records
            .iter()
            .map(|r| r.agent)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        agents.sort_by_key(|a| format!("{a:?}"));

        let mut board: Vec<StrategyPerformance> = agents
            .into_iter()
            .map(|a| self.agent_performance(a))
            .collect();
        board.sort_by(|a, b| {
            a.avg_brier_score
                .partial_cmp(&b.avg_brier_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        board
    }

    pub fn unresolved_predictions(&self) -> Vec<&PredictionRecord> {
        self.records.iter().filter(|r| !r.resolved).collect()
    }

    pub fn domain_calibration(&self, domain: &str) -> f64 {
        let resolved: Vec<&PredictionRecord> = self
            .records
            .iter()
            .filter(|r| r.domain == domain && r.resolved)
            .collect();

        if resolved.is_empty() {
            return 1.0;
        }

        let sum: f64 = resolved
            .iter()
            .map(|r| {
                let actual = r.actual_outcome.unwrap_or(0.0);
                (r.predicted_outcome - actual).abs()
            })
            .sum();
        sum / resolved.len() as f64
    }

    pub fn entries(&self) -> &[PredictionRecord] {
        &self.records
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_resolve_prediction() {
        let mut ledger = PredictionMarketLedger::new(100);
        let id =
            ledger.record_prediction(AgentKind::Strategy, "stability".into(), 0.8, 0.15, 0.2, 10);

        ledger.resolve_prediction(id, 1.0, 20);

        let record = ledger.entries().iter().find(|r| r.id == id).unwrap();
        assert!(record.resolved);
        assert_eq!(record.actual_outcome, Some(1.0));
        let expected_brier = (0.8 - 1.0_f64).powi(2);
        assert!(
            (record.brier_score.unwrap() - expected_brier).abs() < 1e-10,
            "brier_score should be (0.8 - 1.0)^2 = {}",
            expected_brier
        );
    }

    #[test]
    fn leaderboard_sorts_by_brier() {
        let mut ledger = PredictionMarketLedger::new(100);

        let id_good =
            ledger.record_prediction(AgentKind::Strategy, "stability".into(), 0.9, 0.1, 0.1, 1);
        ledger.resolve_prediction(id_good, 1.0, 2);

        let id_bad =
            ledger.record_prediction(AgentKind::Research, "stability".into(), 0.3, 0.05, 0.05, 3);
        ledger.resolve_prediction(id_bad, 1.0, 4);

        let board = ledger.leaderboard();
        assert_eq!(board.len(), 2);
        assert_eq!(board[0].agent, AgentKind::Strategy);
        assert_eq!(board[1].agent, AgentKind::Research);
        assert!(board[0].avg_brier_score < board[1].avg_brier_score);
    }

    #[test]
    fn domain_calibration_computes() {
        let mut ledger = PredictionMarketLedger::new(100);

        let id1 =
            ledger.record_prediction(AgentKind::Strategy, "resilience".into(), 0.7, 0.1, 0.1, 1);
        ledger.resolve_prediction(id1, 0.9, 2);

        let id2 =
            ledger.record_prediction(AgentKind::Research, "resilience".into(), 0.5, 0.1, 0.1, 3);
        ledger.resolve_prediction(id2, 0.6, 4);

        let cal = ledger.domain_calibration("resilience");
        let expected = f64::midpoint((0.7 - 0.9_f64).abs(), (0.5 - 0.6_f64).abs());
        assert!(
            (cal - expected).abs() < 1e-10,
            "calibration should be {expected}, got {cal}"
        );

        let empty_cal = ledger.domain_calibration("nonexistent");
        assert!((empty_cal - 1.0).abs() < 1e-10);
    }

    #[test]
    fn capacity_eviction() {
        let mut ledger = PredictionMarketLedger::new(3);

        let _id0 = ledger.record_prediction(AgentKind::Strategy, "a".into(), 0.5, 0.1, 0.1, 1);
        let _id1 = ledger.record_prediction(AgentKind::Strategy, "b".into(), 0.5, 0.1, 0.1, 2);
        let _id2 = ledger.record_prediction(AgentKind::Strategy, "c".into(), 0.5, 0.1, 0.1, 3);

        assert_eq!(ledger.entries().len(), 3);
        assert_eq!(ledger.entries()[0].id, 0);

        let _id3 = ledger.record_prediction(AgentKind::Strategy, "d".into(), 0.5, 0.1, 0.1, 4);

        assert_eq!(ledger.entries().len(), 3);
        assert_eq!(ledger.entries()[0].id, 1);
        assert_eq!(ledger.entries()[2].id, 3);
    }
}
