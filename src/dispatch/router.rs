//! Generic event router with handler registration.
//!
//! Subsystems register `EventHandler` implementations once at startup, and
//! callers (peripherals, channels, gateway, cron, ...) call `dispatch()` for
//! each incoming event. The router fans out the event to every handler whose
//! `matches()` predicate returns `true`.
//!
//! ## Design notes
//!
//! - Handlers run **sequentially** (not in parallel) to keep ordering
//!   deterministic and to make audit logs easy to reason about.
//! - Handler errors are caught and converted to `HandlerOutcome::Failed` so a
//!   single broken handler cannot poison the dispatch path.
//! - Registration uses `parking_lot::RwLock` (sync) because registration
//!   typically happens once at startup; reads during dispatch take a fast
//!   read lock and clone the `Arc<dyn EventHandler>` list.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use super::types::{DispatchEvent, DispatchResult, HandlerOutcome};

/// A handler that reacts to dispatch events.
///
/// Implementors should be **idempotent** when possible — the same event may
/// be replayed during tests, retries, or audit reruns.
#[async_trait]
pub trait EventHandler: Send + Sync {
    /// Unique handler name (used in audit logs and result attribution).
    fn name(&self) -> &str;

    /// Whether this handler should process the given event.
    /// Pure function — no side effects, called repeatedly during dispatch.
    fn matches(&self, event: &DispatchEvent) -> bool;

    /// Process the event. Errors are caught by the router and turned into
    /// `HandlerOutcome::Failed`. Handlers that want to express "I matched
    /// but chose not to act" should return `Ok(HandlerOutcome::Skipped)`.
    async fn handle(&self, event: &DispatchEvent) -> anyhow::Result<HandlerOutcome>;
}

/// Generic, application-wide event router.
///
/// Used by:
/// - `src/peripherals/signal.rs` (peripheral GPIO/sensor events)
/// - Future MQTT/webhook/cron callers
pub struct EventRouter {
    handlers: RwLock<Vec<Arc<dyn EventHandler>>>,
}

impl EventRouter {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(Vec::new()),
        }
    }

    /// Register a handler. Order of registration is preserved.
    pub fn register(&self, handler: Arc<dyn EventHandler>) {
        self.handlers.write().push(handler);
    }

    /// Number of registered handlers (for diagnostics).
    pub fn handler_count(&self) -> usize {
        self.handlers.read().len()
    }

    /// Dispatch an event to all matching handlers, collecting their outcomes.
    pub async fn dispatch(&self, event: DispatchEvent) -> DispatchResult {
        // Snapshot matching handlers under the read lock, then drop it before
        // doing any async work (parking_lot guards are not Send-safe across
        // await points).
        let matching: Vec<Arc<dyn EventHandler>> = {
            let guard = self.handlers.read();
            guard
                .iter()
                .filter(|h| h.matches(&event))
                .cloned()
                .collect()
        };

        let mut matched_names = Vec::with_capacity(matching.len());
        let mut outcomes = Vec::with_capacity(matching.len());

        for handler in matching {
            let name = handler.name().to_string();
            let outcome = match handler.handle(&event).await {
                Ok(o) => o,
                Err(e) => HandlerOutcome::Failed {
                    error: e.to_string(),
                },
            };
            matched_names.push(name.clone());
            outcomes.push((name, outcome));
        }

        DispatchResult {
            event_id: event.id,
            matched_handlers: matched_names,
            handler_outcomes: outcomes,
        }
    }
}

impl Default for EventRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::types::{DispatchEvent, EventSource};

    /// Test handler that records every event it receives.
    struct RecordingHandler {
        name: String,
        matches_topic: Option<String>,
        seen: parking_lot::Mutex<Vec<String>>,
    }

    impl RecordingHandler {
        fn new(name: &str, matches_topic: Option<&str>) -> Self {
            Self {
                name: name.into(),
                matches_topic: matches_topic.map(String::from),
                seen: parking_lot::Mutex::new(Vec::new()),
            }
        }

        fn count(&self) -> usize {
            self.seen.lock().len()
        }
    }

    #[async_trait]
    impl EventHandler for RecordingHandler {
        fn name(&self) -> &str {
            &self.name
        }

        fn matches(&self, event: &DispatchEvent) -> bool {
            match &self.matches_topic {
                Some(t) => event.topic.as_deref() == Some(t.as_str()),
                None => true,
            }
        }

        async fn handle(&self, event: &DispatchEvent) -> anyhow::Result<HandlerOutcome> {
            self.seen.lock().push(event.id.clone());
            Ok(HandlerOutcome::Handled {
                summary: format!("{} saw {}", self.name, event.id),
            })
        }
    }

    /// Handler that always errors.
    struct FailingHandler;

    #[async_trait]
    impl EventHandler for FailingHandler {
        fn name(&self) -> &str {
            "failing"
        }
        fn matches(&self, _: &DispatchEvent) -> bool {
            true
        }
        async fn handle(&self, _: &DispatchEvent) -> anyhow::Result<HandlerOutcome> {
            anyhow::bail!("intentional test failure")
        }
    }

    #[tokio::test]
    async fn dispatch_to_matching_handler() {
        let router = EventRouter::new();
        let h1 = Arc::new(RecordingHandler::new("h1", Some("nucleo/pin_3")));
        router.register(h1.clone());

        let event = DispatchEvent::new(
            EventSource::Peripheral,
            Some("nucleo/pin_3".into()),
            Some("1".into()),
        );
        let result = router.dispatch(event).await;

        assert_eq!(result.handled_count(), 1);
        assert_eq!(h1.count(), 1);
    }

    #[tokio::test]
    async fn skip_non_matching_handler() {
        let router = EventRouter::new();
        let h1 = Arc::new(RecordingHandler::new("h1", Some("topic_a")));
        let h2 = Arc::new(RecordingHandler::new("h2", Some("topic_b")));
        router.register(h1.clone());
        router.register(h2.clone());

        let event = DispatchEvent::new(EventSource::Manual, Some("topic_a".into()), None);
        router.dispatch(event).await;

        assert_eq!(h1.count(), 1);
        assert_eq!(h2.count(), 0);
    }

    #[tokio::test]
    async fn failing_handler_does_not_poison_dispatch() {
        let router = EventRouter::new();
        router.register(Arc::new(FailingHandler));
        let h2 = Arc::new(RecordingHandler::new("h2", None));
        router.register(h2.clone());

        let event = DispatchEvent::new(EventSource::Manual, None, None);
        let result = router.dispatch(event).await;

        assert_eq!(result.failed_count(), 1);
        assert_eq!(result.handled_count(), 1);
        assert_eq!(h2.count(), 1);
    }

    #[tokio::test]
    async fn handler_count_tracks_registrations() {
        let router = EventRouter::new();
        assert_eq!(router.handler_count(), 0);
        router.register(Arc::new(RecordingHandler::new("a", None)));
        router.register(Arc::new(RecordingHandler::new("b", None)));
        assert_eq!(router.handler_count(), 2);
    }
}
