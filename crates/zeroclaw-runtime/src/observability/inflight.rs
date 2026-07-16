use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Mutex;

#[cfg(test)]
use parking_lot::Mutex as PlMutex;

use super::traits::{Observer, ObserverEvent, ObserverMetric};

/// Process-wide registry tracking the number of in-flight (currently executing)
/// turns per agent alias. Incremented on [`ObserverEvent::AgentStart`],
/// decremented on [`ObserverEvent::AgentEnd`]. The decrement saturates at 0
/// so a stray or duplicate end event cannot drive the count negative.
///
/// The registry is a process-wide singleton — every [`TeeObserver`] and
/// [`InFlightObserver`] instance shares the same underlying map.
static REGISTRY: Mutex<Option<&'static RwLock<InFlightRegistryInner>>> = Mutex::new(None);

struct InFlightRegistryInner {
    counts: HashMap<String, i64>,
}

/// Handle for reading and writing the in-flight turn counts.
///
/// Clone is cheap (clones an `Arc`-like reference to the singleton).
#[derive(Clone, Default)]
pub struct InFlightRegistry {
    _private: (),
}

impl InFlightRegistry {
    /// Increment the in-flight count for `alias`.
    pub fn inc(&self, alias: &str) {
        let mut inner = registry_inner().write();
        *inner.counts.entry(alias.to_string()).or_insert(0) += 1;
    }

    /// Decrement the in-flight count for `alias`, saturating at 0.
    pub fn dec(&self, alias: &str) {
        let mut inner = registry_inner().write();
        if let Some(count) = inner.counts.get_mut(alias) {
            *count = (*count - 1).max(0);
        }
    }

    /// Current in-flight count for a single alias (0 if unseen).
    pub fn get(&self, alias: &str) -> i64 {
        let inner = registry_inner().read();
        inner.counts.get(alias).copied().unwrap_or(0)
    }

    /// Snapshot of all alias → count pairs.
    pub fn snapshot(&self) -> HashMap<String, i64> {
        let inner = registry_inner().read();
        inner.counts.clone()
    }
}

fn registry_inner() -> &'static parking_lot::RwLock<InFlightRegistryInner> {
    let mut lock = REGISTRY.lock().unwrap();
    if lock.is_none() {
        *lock = Some(Box::leak(Box::new(RwLock::new(InFlightRegistryInner {
            counts: HashMap::new(),
        }))));
    }
    lock.unwrap()
}

#[cfg(test)]
pub(crate) static INFLIGHT_TEST_LOCK: PlMutex<()> = PlMutex::new(());

#[cfg(test)]
pub(crate) fn registry_clear_for_test() {
    *REGISTRY.lock().unwrap() = None;
}

/// Observer that updates the in-flight registry on `AgentStart` / `AgentEnd`.
///
/// Intended to be installed as a broadcast hook so that **every** observer in
/// the process — gateway, runtime, channels — contributes to the same count.
#[derive(Default)]
pub struct InFlightObserver {
    registry: InFlightRegistry,
}

impl InFlightObserver {
    pub fn new() -> Self {
        Self::default()
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

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn name(&self) -> &str {
        "inflight"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Return the process-wide [`InFlightRegistry`] instance.
pub fn get_inflight_registry() -> InFlightRegistry {
    InFlightRegistry::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inc_dec_basic() {
        let _lock = INFLIGHT_TEST_LOCK.lock();
        registry_clear_for_test();
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
        let _lock = INFLIGHT_TEST_LOCK.lock();
        let reg = InFlightRegistry::default();
        // Make alias unique per test run to avoid cross-test pollution
        let alias = "sat_unique";
        reg.dec(alias);
        assert_eq!(reg.get(alias), 0);
        reg.dec(alias);
        assert_eq!(reg.get(alias), 0);
    }

    #[test]
    fn snapshot_contains_all_aliases() {
        let _lock = INFLIGHT_TEST_LOCK.lock();
        registry_clear_for_test();
        let reg = InFlightRegistry::default();
        reg.inc("x");
        reg.inc("y");
        reg.inc("y");
        let snap = reg.snapshot();
        assert_eq!(*snap.get("x").unwrap(), 1);
        assert_eq!(*snap.get("y").unwrap(), 2);
    }

    #[test]
    fn observer_inc_on_start_dec_on_end() {
        let _lock = INFLIGHT_TEST_LOCK.lock();
        registry_clear_for_test();
        let obs = InFlightObserver::new();
        let reg = InFlightRegistry::default();
        let alias = "obs_test";

        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            channel: None,
            agent_alias: Some(alias.into()),
            turn_id: None,
        });
        assert_eq!(reg.get(alias), 1);

        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            duration: std::time::Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
            channel: None,
            agent_alias: Some(alias.into()),
            turn_id: None,
        });
        assert_eq!(reg.get(alias), 0);
    }

    #[test]
    fn observer_ignores_events_without_alias() {
        let _lock = INFLIGHT_TEST_LOCK.lock();
        registry_clear_for_test();
        let obs = InFlightObserver::new();
        let reg = InFlightRegistry::default();

        obs.record_event(&ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-4".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
        });
        // Should not crash; simply ignored
        assert_eq!(reg.snapshot().len(), 0);
    }

    #[test]
    fn observer_ignores_unrelated_events() {
        let obs = InFlightObserver::new();
        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_metric(&ObserverMetric::TokensUsed(10));
        // No panic, no state change
    }
}
