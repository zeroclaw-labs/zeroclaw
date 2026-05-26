use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Complete,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRecord {
    pub id: String,
    pub goal: String,
    pub steps: Vec<String>,
    pub status: PlanStatus,
    pub created_at: u64,
    pub source_id: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanningMemory {
    pub plans: HashMap<String, PlanRecord>,
}

impl PlanningMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&mut self, id: &str, goal: &str, steps: Vec<String>, tick: u64) {
        self.plans.insert(
            id.to_string(),
            PlanRecord {
                id: id.to_string(),
                goal: goal.to_string(),
                steps,
                status: PlanStatus::Pending,
                created_at: tick,
                source_id: String::new(),
                confidence: 1.0,
            },
        );
    }

    pub fn retrieve(&self, id: &str) -> Option<&PlanRecord> {
        self.plans.get(id)
    }

    pub fn update_status(&mut self, id: &str, status: PlanStatus) {
        if let Some(plan) = self.plans.get_mut(id) {
            plan.status = status;
        }
    }

    pub fn active_plans(&self) -> Vec<&PlanRecord> {
        self.plans
            .values()
            .filter(|p| p.status == PlanStatus::Pending || p.status == PlanStatus::InProgress)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve() {
        let mut pm = PlanningMemory::new();
        pm.store(
            "p1",
            "build feature",
            vec!["design".into(), "code".into()],
            1,
        );
        let plan = pm.retrieve("p1").unwrap();
        assert_eq!(plan.goal, "build feature");
        assert_eq!(plan.status, PlanStatus::Pending);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn update_status_filters_active() {
        let mut pm = PlanningMemory::new();
        pm.store("p1", "a", vec![], 1);
        pm.store("p2", "b", vec![], 2);
        pm.update_status("p1", PlanStatus::Complete);
        assert_eq!(pm.active_plans().len(), 1);
    }
}
