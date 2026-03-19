use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Status of an individual agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is running and responsive.
    Healthy,
    /// Agent is running but hasn't responded to recent pings.
    Unresponsive { consecutive_failures: u32 },
    /// Agent hasn't made progress beyond the stuck threshold.
    Stuck { since: Instant },
    /// Agent was killed (either by health monitor or manually).
    Dead { reason: String },
    /// Agent is starting up or restarting.
    Starting,
}

/// Health state for a single agent.
#[derive(Debug, Clone)]
pub struct AgentHealth {
    pub name: String,
    pub role: String,
    pub status: AgentStatus,
    pub last_ping: Option<Instant>,
    pub last_activity: Option<Instant>,
    pub started_at: Instant,
    pub kill_count: u32,
    pub last_killed_at: Option<Instant>,
}

impl AgentHealth {
    pub fn new(name: String, role: String) -> Self {
        Self {
            name,
            role,
            status: AgentStatus::Starting,
            last_ping: None,
            last_activity: None,
            started_at: Instant::now(),
            kill_count: 0,
            last_killed_at: None,
        }
    }

    /// Record a successful health check.
    pub fn record_ping(&mut self) {
        self.last_ping = Some(Instant::now());
        self.status = AgentStatus::Healthy;
    }

    /// Record activity from the agent (tool call, message, etc.).
    pub fn record_activity(&mut self) {
        self.last_activity = Some(Instant::now());
    }

    /// Check health against thresholds.
    pub fn evaluate(
        &mut self,
        ping_timeout: Duration,
        max_consecutive_failures: u32,
        stuck_threshold: Duration,
    ) {
        let now = Instant::now();

        // Check stuck threshold
        let last_active = self.last_activity.unwrap_or(self.started_at);
        if now.duration_since(last_active) > stuck_threshold {
            self.status = AgentStatus::Stuck { since: last_active };
            return;
        }

        // Check ping timeout
        if let Some(last_ping) = self.last_ping {
            if now.duration_since(last_ping) > ping_timeout {
                let failures = match &self.status {
                    AgentStatus::Unresponsive {
                        consecutive_failures,
                    } => consecutive_failures + 1,
                    _ => 1,
                };
                if failures >= max_consecutive_failures {
                    self.status = AgentStatus::Dead {
                        reason: format!(
                            "{failures} consecutive ping failures (threshold: {max_consecutive_failures})"
                        ),
                    };
                } else {
                    self.status = AgentStatus::Unresponsive {
                        consecutive_failures: failures,
                    };
                }
            }
        }
    }

    /// Whether this agent can be restarted (respecting kill cooldown).
    pub fn can_restart(&self, kill_cooldown: Duration) -> bool {
        match self.last_killed_at {
            Some(killed_at) => Instant::now().duration_since(killed_at) > kill_cooldown,
            None => true,
        }
    }

    /// Record that this agent was killed.
    pub fn record_kill(&mut self, reason: String) {
        self.kill_count += 1;
        self.last_killed_at = Some(Instant::now());
        self.status = AgentStatus::Dead { reason };
    }
}

/// Health monitor that tracks multiple agents.
pub struct HealthMonitor {
    agents: RwLock<HashMap<String, AgentHealth>>,
}

impl HealthMonitor {
    pub fn new() -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new agent for monitoring.
    pub async fn register(&self, name: String, role: String) {
        let health = AgentHealth::new(name.clone(), role);
        self.agents.write().await.insert(name, health);
    }

    /// Remove an agent from monitoring.
    pub async fn unregister(&self, name: &str) {
        self.agents.write().await.remove(name);
    }

    /// Record a successful ping for an agent.
    pub async fn record_ping(&self, name: &str) {
        if let Some(health) = self.agents.write().await.get_mut(name) {
            health.record_ping();
        }
    }

    /// Record activity for an agent.
    pub async fn record_activity(&self, name: &str) {
        if let Some(health) = self.agents.write().await.get_mut(name) {
            health.record_activity();
        }
    }

    /// Get the health status of all agents.
    pub async fn snapshot(&self) -> Vec<AgentHealth> {
        self.agents.read().await.values().cloned().collect()
    }

    /// Get agents that need to be killed (dead status).
    pub async fn dead_agents(&self) -> Vec<String> {
        self.agents
            .read()
            .await
            .iter()
            .filter(|(_, h)| matches!(h.status, AgentStatus::Dead { .. }))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get agents that are stuck.
    pub async fn stuck_agents(&self) -> Vec<String> {
        self.agents
            .read()
            .await
            .iter()
            .filter(|(_, h)| matches!(h.status, AgentStatus::Stuck { .. }))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Run one evaluation cycle for all agents.
    pub async fn evaluate_all(
        &self,
        ping_timeout: Duration,
        max_failures: u32,
        stuck_threshold: Duration,
    ) {
        let mut agents = self.agents.write().await;
        for health in agents.values_mut() {
            health.evaluate(ping_timeout, max_failures, stuck_threshold);
        }
    }
}

impl Default for HealthMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_agent_starts_in_starting_state() {
        let health = AgentHealth::new("test".into(), "crew".into());
        assert_eq!(health.status, AgentStatus::Starting);
        assert_eq!(health.kill_count, 0);
    }

    #[test]
    fn ping_transitions_to_healthy() {
        let mut health = AgentHealth::new("test".into(), "crew".into());
        health.record_ping();
        assert_eq!(health.status, AgentStatus::Healthy);
    }

    #[test]
    fn kill_cooldown_respected() {
        let mut health = AgentHealth::new("test".into(), "crew".into());
        health.record_kill("test kill".into());
        assert!(!health.can_restart(Duration::from_secs(300)));
    }

    #[tokio::test]
    async fn monitor_register_and_snapshot() {
        let monitor = HealthMonitor::new();
        monitor.register("agent-1".into(), "crew".into()).await;
        monitor.register("agent-2".into(), "mayor".into()).await;
        let snapshot = monitor.snapshot().await;
        assert_eq!(snapshot.len(), 2);
    }
}
