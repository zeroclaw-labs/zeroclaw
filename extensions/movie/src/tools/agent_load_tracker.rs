//! Shared runtime load tracker for team/subagent orchestration.
//!
//! The tracker records in-flight counts and recent assignment/failure events
//! per agent. Selection logic can then apply dynamic load-aware penalties
//! without hardcoding specific agent identities.

use parking_lot::RwLock;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIN_RETENTION: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentLoadSnapshot {
    pub in_flight: usize,
    pub recent_assignments: usize,
    pub recent_failures: usize,
}

#[derive(Debug, Default)]
struct AgentRuntimeLoad {
    in_flight: usize,
    assignment_events: VecDeque<Instant>,
    failure_events: VecDeque<Instant>,
}

#[derive(Clone, Default)]
pub struct AgentLoadTracker {
    inner: Arc<RwLock<HashMap<String, AgentRuntimeLoad>>>,
}

impl AgentLoadTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark an assignment as started and return a lease that must be finalized.
    pub fn start(&self, agent_name: &str) -> AgentLoadLease {
        let agent = agent_name.trim();
        if agent.is_empty() {
            return AgentLoadLease::noop(self.clone());
        }

        let now = Instant::now();
        let mut map = self.inner.write();
        let state = map.entry(agent.to_string()).or_default();
        state.in_flight = state.in_flight.saturating_add(1);
        state.assignment_events.push_back(now);
        Self::prune_state(state, now, Duration::from_secs(600));

        AgentLoadLease {
            tracker: self.clone(),
            agent_name: agent.to_string(),
            finalized: false,
            active: true,
        }
    }

    /// Record a direct failure (for example provider creation failure).
    pub fn record_failure(&self, agent_name: &str) {
        let agent = agent_name.trim();
        if agent.is_empty() {
            return;
        }

        let now = Instant::now();
        let mut map = self.inner.write();
        let state = map.entry(agent.to_string()).or_default();
        state.failure_events.push_back(now);
        Self::prune_state(state, now, Duration::from_secs(600));
    }

    /// Return current load snapshots using the provided recent-event window.
    pub fn snapshot(&self, window: Duration) -> HashMap<String, AgentLoadSnapshot> {
        let effective_window = if window.is_zero() {
            Duration::from_secs(1)
        } else {
            window
        };
        let retention = effective_window.checked_mul(4).unwrap_or(effective_window);
        let retention = retention.max(MIN_RETENTION);
        let now = Instant::now();

        let mut map = self.inner.write();
        let mut out = HashMap::new();
        for (agent, state) in map.iter_mut() {
            Self::prune_state(state, now, retention);
            let recent_assignments = state
                .assignment_events
                .iter()
                .filter(|timestamp| now.saturating_duration_since(**timestamp) <= effective_window)
                .count();
            let recent_failures = state
                .failure_events
                .iter()
                .filter(|timestamp| now.saturating_duration_since(**timestamp) <= effective_window)
                .count();
            out.insert(
                agent.clone(),
                AgentLoadSnapshot {
                    in_flight: state.in_flight,
                    recent_assignments,
                    recent_failures,
                },
            );
        }
        out
    }

    fn finish(&self, agent_name: &str, success: bool) {
        let agent = agent_name.trim();
        if agent.is_empty() {
            return;
        }

        let now = Instant::now();
        let mut map = self.inner.write();
        let state = map.entry(agent.to_string()).or_default();
        state.in_flight = state.in_flight.saturating_sub(1);
        if !success {
            state.failure_events.push_back(now);
        }
        Self::prune_state(state, now, Duration::from_secs(600));
    }

    fn prune_state(state: &mut AgentRuntimeLoad, now: Instant, retention: Duration) {
        while state
            .assignment_events
            .front()
            .is_some_and(|timestamp| now.saturating_duration_since(*timestamp) > retention)
        {
            state.assignment_events.pop_front();
        }
        while state
            .failure_events
            .front()
            .is_some_and(|timestamp| now.saturating_duration_since(*timestamp) > retention)
        {
            state.failure_events.pop_front();
        }
    }
}

pub struct AgentLoadLease {
    tracker: AgentLoadTracker,
    agent_name: String,
    finalized: bool,
    active: bool,
}

impl AgentLoadLease {
    fn noop(tracker: AgentLoadTracker) -> Self {
        Self {
            tracker,
            agent_name: String::new(),
            finalized: true,
            active: false,
        }
    }

    pub fn mark_success(&mut self) {
        if !self.active || self.finalized {
            return;
        }
        self.tracker.finish(&self.agent_name, true);
        self.finalized = true;
    }

    pub fn mark_failure(&mut self) {
        if !self.active || self.finalized {
            return;
        }
        self.tracker.finish(&self.agent_name, false);
        self.finalized = true;
    }
}

impl Drop for AgentLoadLease {
    fn drop(&mut self) {
        if !self.active || self.finalized {
            return;
        }
        self.tracker.finish(&self.agent_name, false);
        self.finalized = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reflects_inflight_and_completion() {
        let tracker = AgentLoadTracker::new();
        let mut lease = tracker.start("coder");

        let snap = tracker.snapshot(Duration::from_secs(60));
        assert_eq!(snap.get("coder").map(|entry| entry.in_flight), Some(1));
        assert_eq!(
            snap.get("coder").map(|entry| entry.recent_assignments),
            Some(1)
        );

        lease.mark_success();

        let snap = tracker.snapshot(Duration::from_secs(60));
        assert_eq!(snap.get("coder").map(|entry| entry.in_flight), Some(0));
        assert_eq!(
            snap.get("coder").map(|entry| entry.recent_failures),
            Some(0)
        );
    }

    #[test]
    fn dropped_lease_marks_failure_and_releases_inflight() {
        let tracker = AgentLoadTracker::new();
        {
            let _lease = tracker.start("researcher");
        }

        let snap = tracker.snapshot(Duration::from_secs(60));
        assert_eq!(snap.get("researcher").map(|entry| entry.in_flight), Some(0));
        assert_eq!(
            snap.get("researcher").map(|entry| entry.recent_failures),
            Some(1)
        );
    }

    #[test]
    fn record_failure_without_start_is_counted() {
        let tracker = AgentLoadTracker::new();
        tracker.record_failure("planner");

        let snap = tracker.snapshot(Duration::from_secs(60));
        assert_eq!(snap.get("planner").map(|entry| entry.in_flight), Some(0));
        assert_eq!(
            snap.get("planner").map(|entry| entry.recent_failures),
            Some(1)
        );
    }
}
