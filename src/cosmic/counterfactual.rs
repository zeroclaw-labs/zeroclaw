use std::collections::HashMap;

use chrono::{DateTime, Utc};
use num_complex::Complex64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub action: String,
    pub context: HashMap<String, f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub scenario_id: String,
    pub predicted_outcome: f64,
    pub confidence: f64,
    pub risk: f64,
    pub affected_subsystems: Vec<String>,
    pub simulated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CounterfactualEngine {
    world_beliefs: HashMap<String, f64>,
    simulations: Vec<SimulationResult>,
    max_scenarios: usize,
    history_capacity: usize,
}

impl CounterfactualEngine {
    pub fn new(max_scenarios: usize, history_capacity: usize) -> Self {
        Self {
            world_beliefs: HashMap::new(),
            simulations: Vec::new(),
            max_scenarios,
            history_capacity,
        }
    }

    pub fn update_world_state(&mut self, key: &str, value: f64) {
        self.world_beliefs.insert(key.to_string(), value);
    }

    pub fn simulate(&mut self, scenario: &Scenario) -> SimulationResult {
        let affected_subsystems: Vec<String> = scenario
            .context
            .keys()
            .filter(|k| self.world_beliefs.contains_key(*k))
            .cloned()
            .collect();

        let overlap = affected_subsystems.len();
        let total_context = scenario.context.len().max(1);
        let confidence = overlap as f64 / total_context as f64;

        let mut divergence_sum = 0.0;
        let mut divergence_count = 0usize;
        for (key, ctx_val) in &scenario.context {
            if let Some(world_val) = self.world_beliefs.get(key) {
                divergence_sum += (ctx_val - world_val).abs();
                divergence_count += 1;
            }
        }
        let risk = if divergence_count > 0 {
            (divergence_sum / divergence_count as f64).clamp(0.0, 1.0)
        } else {
            0.5
        };

        let mut alignment_sum = 0.0;
        let mut alignment_count = 0usize;
        for (key, ctx_val) in &scenario.context {
            if let Some(world_val) = self.world_beliefs.get(key) {
                alignment_sum += 1.0 - (ctx_val - world_val).abs().min(1.0);
                alignment_count += 1;
            }
        }
        let predicted_outcome = if alignment_count > 0 {
            alignment_sum / alignment_count as f64
        } else {
            0.5
        };

        let result = SimulationResult {
            scenario_id: scenario.id.clone(),
            predicted_outcome,
            confidence,
            risk,
            affected_subsystems,
            simulated_at: Utc::now(),
        };

        self.simulations.push(result.clone());
        if self.simulations.len() > self.history_capacity {
            self.simulations.remove(0);
        }

        result
    }

    pub fn compare_scenarios(&mut self, scenarios: &[Scenario]) -> Vec<SimulationResult> {
        let mut results: Vec<SimulationResult> =
            scenarios.iter().map(|s| self.simulate(s)).collect();
        results.sort_by(|a, b| {
            b.predicted_outcome
                .partial_cmp(&a.predicted_outcome)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results
    }

    pub fn best_action(&mut self, scenarios: &[Scenario]) -> Option<SimulationResult> {
        let results = self.compare_scenarios(scenarios);
        results.into_iter().find(|r| r.risk < 0.7)
    }

    pub fn regret(&self, scenario_id: &str, actual_outcome: f64) -> Option<f64> {
        self.simulations
            .iter()
            .find(|s| s.scenario_id == scenario_id)
            .map(|s| (s.predicted_outcome - actual_outcome).abs())
    }

    pub fn simulation_count(&self) -> usize {
        self.simulations.len()
    }

    pub fn path_integral_select(
        &mut self,
        scenarios: &[Scenario],
        rng: &mut dyn rand::RngCore,
    ) -> Option<SimulationResult> {
        if scenarios.is_empty() {
            return None;
        }

        let results: Vec<SimulationResult> = scenarios.iter().map(|s| self.simulate(s)).collect();

        let hbar = 0.1;
        let amplitudes: Vec<Complex64> = results
            .iter()
            .map(|r| {
                let action = r.risk + (1.0 - r.predicted_outcome);
                Complex64::from_polar(1.0, action / hbar)
            })
            .collect();

        let total_norm_sq: f64 = amplitudes.iter().map(|a| a.norm_sqr()).sum();
        if total_norm_sq < 1e-15 {
            return results.into_iter().next();
        }

        let probabilities: Vec<f64> = amplitudes
            .iter()
            .map(|a| a.norm_sqr() / total_norm_sq)
            .collect();

        use rand::Rng;
        let r: f64 = rng.random();
        let mut cumulative = 0.0;
        for (i, p) in probabilities.iter().enumerate() {
            cumulative += p;
            if r < cumulative {
                return Some(results[i].clone());
            }
        }

        results.into_iter().last()
    }

    pub fn clear_history(&mut self) {
        self.simulations.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_scenario(id: &str, context: Vec<(&str, f64)>) -> Scenario {
        Scenario {
            id: id.to_string(),
            action: format!("action_{id}"),
            context: context
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn simulate_single_scenario() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("energy", 0.8);
        engine.update_world_state("load", 0.3);
        let scenario = make_scenario("s1", vec![("energy", 0.7), ("load", 0.4)]);
        let result = engine.simulate(&scenario);
        assert_eq!(result.scenario_id, "s1");
        assert!(result.predicted_outcome >= 0.0 && result.predicted_outcome <= 1.0);
        assert!(result.confidence >= 0.0 && result.confidence <= 1.0);
        assert!(result.risk >= 0.0 && result.risk <= 1.0);
        assert_eq!(result.affected_subsystems.len(), 2);
    }

    #[test]
    fn compare_scenarios_sorts_descending() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("x", 0.9);
        let good = make_scenario("good", vec![("x", 0.9)]);
        let bad = make_scenario("bad", vec![("x", 0.1)]);
        let results = engine.compare_scenarios(&[bad, good]);
        assert!(results[0].predicted_outcome >= results[1].predicted_outcome);
    }

    #[test]
    fn best_action_filters_high_risk() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("x", 0.5);
        let risky = make_scenario("risky", vec![("x", 1.0)]);
        let safe = make_scenario("safe", vec![("x", 0.5)]);
        let best = engine.best_action(&[risky, safe]);
        assert!(best.is_some());
        let best = best.unwrap();
        assert!(best.risk < 0.7);
    }

    #[test]
    fn regret_calculation() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("x", 0.5);
        let scenario = make_scenario("s1", vec![("x", 0.5)]);
        let result = engine.simulate(&scenario);
        let regret = engine.regret("s1", 0.3).unwrap();
        assert!((regret - (result.predicted_outcome - 0.3).abs()).abs() < f64::EPSILON);
    }

    #[test]
    fn regret_unknown_scenario_returns_none() {
        let engine = CounterfactualEngine::new(10, 100);
        assert!(engine.regret("nonexistent", 0.5).is_none());
    }

    #[test]
    fn update_world_state_affects_simulation() {
        let mut engine = CounterfactualEngine::new(10, 100);
        let scenario = make_scenario("s1", vec![("x", 0.9)]);

        engine.update_world_state("x", 0.1);
        let r1 = engine.simulate(&scenario);

        engine.update_world_state("x", 0.9);
        let r2 = engine.simulate(&scenario);

        assert!(r2.predicted_outcome > r1.predicted_outcome);
    }

    #[test]
    fn empty_engine_safe_defaults() {
        let mut engine = CounterfactualEngine::new(10, 100);
        let scenario = make_scenario("s1", vec![("x", 0.5)]);
        let result = engine.simulate(&scenario);
        assert!((result.predicted_outcome - 0.5).abs() < f64::EPSILON);
        assert!((result.confidence - 0.0).abs() < f64::EPSILON);
        assert!((result.risk - 0.5).abs() < f64::EPSILON);
        assert!(result.affected_subsystems.is_empty());
    }

    #[test]
    fn history_capacity_pruning() {
        let mut engine = CounterfactualEngine::new(10, 3);
        engine.update_world_state("x", 0.5);
        for i in 0..5 {
            let scenario = make_scenario(&format!("s{i}"), vec![("x", 0.5)]);
            engine.simulate(&scenario);
        }
        assert_eq!(engine.simulation_count(), 3);
    }

    #[test]
    fn clear_history_works() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("x", 0.5);
        let scenario = make_scenario("s1", vec![("x", 0.5)]);
        engine.simulate(&scenario);
        assert_eq!(engine.simulation_count(), 1);
        engine.clear_history();
        assert_eq!(engine.simulation_count(), 0);
    }

    #[test]
    fn simulate_with_no_world_state() {
        let mut engine = CounterfactualEngine::new(10, 100);
        let scenario = make_scenario("s1", vec![]);
        let result = engine.simulate(&scenario);
        assert!((result.predicted_outcome - 0.5).abs() < f64::EPSILON);
        assert_eq!(engine.simulation_count(), 1);
    }

    #[test]
    fn path_integral_selects_scenario() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("x", 0.8);
        engine.update_world_state("y", 0.5);
        let scenarios = vec![
            make_scenario("good", vec![("x", 0.8), ("y", 0.5)]),
            make_scenario("bad", vec![("x", 0.1), ("y", 0.9)]),
            make_scenario("mid", vec![("x", 0.6), ("y", 0.4)]),
        ];
        let mut rng = rand::rng();
        let result = engine.path_integral_select(&scenarios, &mut rng);
        assert!(result.is_some());
    }

    #[test]
    fn path_integral_empty_scenarios() {
        let mut engine = CounterfactualEngine::new(10, 100);
        let mut rng = rand::rng();
        let result = engine.path_integral_select(&[], &mut rng);
        assert!(result.is_none());
    }

    #[test]
    fn path_integral_single_scenario() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("x", 0.5);
        let scenarios = vec![make_scenario("only", vec![("x", 0.5)])];
        let mut rng = rand::rng();
        let result = engine.path_integral_select(&scenarios, &mut rng);
        assert!(result.is_some());
        assert_eq!(result.unwrap().scenario_id, "only");
    }

    #[test]
    fn max_scenarios_does_not_crash() {
        let mut engine = CounterfactualEngine::new(2, 100);
        engine.update_world_state("x", 0.5);
        let scenarios: Vec<Scenario> = (0..5)
            .map(|i| make_scenario(&format!("s{i}"), vec![("x", 0.5)]))
            .collect();
        let results = engine.compare_scenarios(&scenarios);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn perfect_alignment_yields_high_outcome() {
        let mut engine = CounterfactualEngine::new(10, 100);
        engine.update_world_state("a", 0.5);
        engine.update_world_state("b", 0.8);
        let scenario = make_scenario("perfect", vec![("a", 0.5), ("b", 0.8)]);
        let result = engine.simulate(&scenario);
        assert!((result.predicted_outcome - 1.0).abs() < f64::EPSILON);
        assert!((result.risk - 0.0).abs() < f64::EPSILON);
    }
}
