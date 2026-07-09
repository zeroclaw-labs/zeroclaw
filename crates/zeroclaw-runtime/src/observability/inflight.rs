use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use super::traits::{Observer, ObserverEvent};

/// Process-wide registry of per-agent in-flight turn counts.
///
/// Incremented on [`ObserverEvent::AgentStart`], decremented (saturating at 0)
/// on [`ObserverEvent::AgentEnd`]. Because the end event fires from
/// [`crate::agent::TurnGuard`]`::drop`, the decrement is guaranteed on every
/// exit path — normal return, early return, and error/panic unwind.
///
/// Accessed via [`in_flight_registry()`] so both the observability pipeline
/// (writer) and the gateway API (reader) share the same instance.
#[derive(Clone, Default)]
pub struct InFlightRegistry {
    counts: Arc<RwLock<HashMap<String, i64>>>,
}

impl InFlightRegistry {
    /// Atomically increment the in-flight count for `alias`.
    pub fn inc(&self, alias: &str) {
        let mut map = self.counts.write();
        *map.entry(alias.to_string()).or_insert(0) += 1;
    }

    /// Atomically decrement the in-flight count for `alias`, saturating at 0.
    pub fn dec(&self, alias: &str) {
        let mut map = self.counts.write();
        if let Some(v) = map.get_mut(alias) {
            *v = (*v - 1).max(0);
        }
    }

    /// Current in-flight count for a single alias (0 if unseen).
    pub fn get(&self, alias: &str) -> i64 {
        self.counts.read().get(alias).copied().unwrap_or(0)
    }

    /// Snapshot of all non-zero per-alias counts.
    pub fn snapshot(&self) -> HashMap<String, i64> {
        let map = self.counts.read();
        map.iter()
            .filter(|(_, v)| **v > 0)
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    /// Sum of all per-alias in-flight counts.
    pub fn total(&self) -> i64 {
        self.counts.read().values().sum()
    }
}

/// Global singleton registry shared between the observability pipeline and the
/// gateway. Initialized lazily on first call.
static IN_FLIGHT_REGISTRY: OnceLock<InFlightRegistry> = OnceLock::new();

/// Return a clone of the process-wide in-flight registry.
pub fn in_flight_registry() -> InFlightRegistry {
    IN_FLIGHT_REGISTRY
        .get_or_init(InFlightRegistry::default)
        .clone()
}

/// Reset the global registry. Intended for tests only.
#[cfg(test)]
pub(crate) fn reset_in_flight_registry() {
    if let Some(reg) = IN_FLIGHT_REGISTRY.get() {
        let mut map = reg.counts.write();
        map.clear();
    }
}

/// Observer that maintains the in-flight per-agent turn counter.
///
/// Typically composed into the observer chain via [`TeeObserver`] so that
/// every [`AgentStart`] / [`AgentEnd`] event automatically updates the
/// registry.
#[derive(Clone)]
pub struct InFlightObserver {
    registry: InFlightRegistry,
}

impl Default for InFlightObserver {
    fn default() -> Self {
        Self {
            registry: in_flight_registry(),
        }
    }
}

impl InFlightObserver {
    fn with_registry(reg: InFlightRegistry) -> Self {
        Self { registry: reg }
    }
}

impl Observer for InFlightObserver {
    fn record_event(&self, event: &ObserverEvent) {
        match event {
            ObserverEvent::AgentStart {
                agent_alias: Some(alias),
                ..
            } => {
                self.registry.inc(alias);
            }
            ObserverEvent::AgentEnd {
                agent_alias: Some(alias),
                ..
            } => {
                self.registry.dec(alias);
            }
            _ => {}
        }
    }

    fn record_metric(&self, _metric: &super::traits::ObserverMetric) {}

    fn name(&self) -> &str {
        "in-flight"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inc_dec_basic() {
        let reg = InFlightRegistry::default();
        assert_eq!(reg.get("alpha"), 0);
        reg.inc("alpha");
        assert_eq!(reg.get("alpha"), 1);
        reg.inc("alpha");
        assert_eq!(reg.get("alpha"), 2);
        reg.dec("alpha");
        assert_eq!(reg.get("alpha"), 1);
        reg.dec("alpha");
        assert_eq!(reg.get("alpha"), 0);
    }

    #[test]
    fn dec_saturates_at_zero() {
        let reg = InFlightRegistry::default();
        reg.dec("unknown");
        assert_eq!(reg.get("unknown"), 0);
        reg.inc("beta");
        reg.dec("beta");
        reg.dec("beta"); // extra decrement
        assert_eq!(reg.get("beta"), 0);
    }

    #[test]
    fn snapshot_filters_zeros() {
        let reg = InFlightRegistry::default();
        reg.inc("a");
        reg.inc("b");
        reg.inc("b");
        reg.dec("a"); // back to 0
        let snap = reg.snapshot();
        assert_eq!(snap.get("b").copied(), Some(2));
        assert!(!snap.contains_key("a"));
    }

    #[test]
    fn total_sums_all_aliases() {
        let reg = InFlightRegistry::default();
        reg.inc("x");
        reg.inc("y");
        reg.inc("y");
        assert_eq!(reg.total(), 3);
    }

    #[test]
    fn observer_maps_start_and_end() {
        let reg = InFlightRegistry::default();
        let obs = InFlightObserver::with_registry(reg.clone());

        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            channel: None,
            agent_alias: Some("coder".into()),
            turn_id: None,
        });
        assert_eq!(reg.get("coder"), 1);

        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            duration: std::time::Duration::from_secs(1),
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: Some("coder".into()),
            turn_id: None,
        });
        assert_eq!(reg.get("coder"), 0);
    }

    #[test]
    fn observer_ignores_missing_alias() {
        let reg = InFlightRegistry::default();
        let obs = InFlightObserver::with_registry(reg.clone());

        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            channel: None,
            agent_alias: None, // no alias
            turn_id: None,
        });
        assert_eq!(reg.total(), 0);
    }

    #[test]
    fn observer_ignores_unrelated_events() {
        let reg = InFlightRegistry::default();
        let obs = InFlightObserver::with_registry(reg.clone());
        reg.inc("z");

        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::TurnComplete);
        assert_eq!(reg.get("z"), 1);
    }

    #[test]
    fn global_registry_is_shared() {
        reset_in_flight_registry();
        let reg1 = in_flight_registry();
        let reg2 = in_flight_registry();
        reg1.inc("shared");
        assert_eq!(reg2.get("shared"), 1);
        reset_in_flight_registry();
    }
}
