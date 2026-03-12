use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::consciousness::traits::Priority;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceGoal {
    pub id: String,
    pub description: String,
    pub priority: Priority,
    pub created_at: DateTime<Utc>,
    pub deadline: Option<DateTime<Utc>>,
    pub status: SceGoalStatus,
    pub parent_id: Option<String>,
    pub progress: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceGoalStatus {
    Pending,
    Active,
    Blocked,
    Completed,
    Abandoned,
}

impl Eq for SceGoal {}

impl PartialEq for SceGoal {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl PartialOrd for SceGoal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SceGoal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority
            .weight()
            .partial_cmp(&other.priority.weight())
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoalStack {
    goals: Vec<SceGoal>,
}

impl GoalStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, goal: SceGoal) {
        self.goals.push(goal);
    }

    pub fn pop_highest(&mut self) -> Option<SceGoal> {
        if self.goals.is_empty() {
            return None;
        }
        let idx = self
            .goals
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.cmp(b))
            .map(|(i, _)| i)?;
        Some(self.goals.remove(idx))
    }

    pub fn peek_highest(&self) -> Option<&SceGoal> {
        self.goals.iter().max_by(|a, b| a.cmp(b))
    }

    pub fn active(&self) -> Vec<&SceGoal> {
        self.goals
            .iter()
            .filter(|g| g.status == SceGoalStatus::Active)
            .collect()
    }

    pub fn pending(&self) -> Vec<&SceGoal> {
        self.goals
            .iter()
            .filter(|g| g.status == SceGoalStatus::Pending)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&SceGoal> {
        self.goals.iter().find(|g| g.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut SceGoal> {
        self.goals.iter_mut().find(|g| g.id == id)
    }

    pub fn complete(&mut self, id: &str) -> bool {
        if let Some(goal) = self.get_mut(id) {
            goal.status = SceGoalStatus::Completed;
            goal.progress = 1.0;
            true
        } else {
            false
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<SceGoal> {
        let idx = self.goals.iter().position(|g| g.id == id)?;
        Some(self.goals.remove(idx))
    }

    pub fn len(&self) -> usize {
        self.goals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.goals.is_empty()
    }

    pub fn all(&self) -> &[SceGoal] {
        &self.goals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_goal(id: &str, priority: Priority) -> SceGoal {
        SceGoal {
            id: id.into(),
            description: format!("goal {id}"),
            priority,
            created_at: Utc::now(),
            deadline: None,
            status: SceGoalStatus::Pending,
            parent_id: None,
            progress: 0.0,
        }
    }

    #[test]
    fn pop_returns_highest_priority() {
        let mut stack = GoalStack::new();
        stack.push(make_goal("low", Priority::Low));
        stack.push(make_goal("crit", Priority::Critical));
        stack.push(make_goal("mid", Priority::Normal));
        let top = stack.pop_highest().unwrap();
        assert_eq!(top.id, "crit");
    }

    #[test]
    fn complete_marks_goal_done() {
        let mut stack = GoalStack::new();
        stack.push(make_goal("g1", Priority::Normal));
        assert!(stack.complete("g1"));
        assert_eq!(stack.get("g1").unwrap().status, SceGoalStatus::Completed);
    }

    #[test]
    fn empty_stack_returns_none() {
        let mut stack = GoalStack::new();
        assert!(stack.pop_highest().is_none());
        assert!(stack.peek_highest().is_none());
    }
}
