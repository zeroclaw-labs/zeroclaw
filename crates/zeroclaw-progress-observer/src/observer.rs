//! Per-message observer that fires status updates into a target channel.
//!
//! Construction is cheap and per-`ChannelMessage`; the orchestrator builds
//! a fresh one per turn and drops it when the turn ends. Emission is
//! fire-and-forget — failures only land in `tracing::warn!`.

use std::sync::Arc;

use zeroclaw_api::channel::Channel;
use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

use crate::mapping::event_to_status;
use crate::toggles::ProgressEventToggles;

/// Sidelined observer that emits one [`StatusUpdate`] per enabled event.
pub struct ProgressReportingObserver {
    inner: Arc<dyn Observer>,
    execution_id: String,
    target_channel: Arc<dyn Channel>,
    recipient: String,
    thread_ts: Option<String>,
    toggles: ProgressEventToggles,
}

impl ProgressReportingObserver {
    /// Build a new observer bound to one channel turn.
    ///
    /// Logs a one-time `info!` diagnostic if every sub-toggle is disabled,
    /// so operators don't wonder why nothing is happening when they enable
    /// the master switch but forget the per-event toggles.
    pub fn new(
        execution_id: String,
        target_channel: Arc<dyn Channel>,
        recipient: String,
        thread_ts: Option<String>,
        toggles: ProgressEventToggles,
        inner: Arc<dyn Observer>,
    ) -> Self {
        if !toggles.any_enabled() {
            tracing::info!(
                target: "zeroclaw::progress_observer",
                "attached with all event toggles disabled; configure \
                 [progress_observer] subkeys to enable"
            );
        }
        Self {
            inner,
            execution_id,
            target_channel,
            recipient,
            thread_ts,
            toggles,
        }
    }
}

impl Observer for ProgressReportingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        let update = event_to_status(&self.execution_id, event, &self.toggles);
        tracing::info!(
            target: "zeroclaw::progress_observer",
            event = ?std::mem::discriminant(event),
            will_emit = update.is_some(),
            "record_event"
        );
        if let Some(update) = update {
            let ch = Arc::clone(&self.target_channel);
            let recip = self.recipient.clone();
            let thread = self.thread_ts.clone();
            tokio::spawn(async move {
                if let Err(e) = ch
                    .send_status_update(&recip, thread.as_deref(), update)
                    .await
                {
                    tracing::warn!(
                        target: "zeroclaw::progress_observer",
                        error = %e,
                        "progress status_update failed"
                    );
                }
            });
        }
        self.inner.record_event(event);
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str { "progress-reporting" }

    fn as_any(&self) -> &dyn std::any::Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use crate::mock::MockChannel;

    struct NoopObserver {
        events: Mutex<usize>,
    }
    impl NoopObserver {
        fn new() -> Self { Self { events: Mutex::new(0) } }
        fn count(&self) -> usize { *self.events.lock().unwrap() }
    }
    impl Observer for NoopObserver {
        fn record_event(&self, _: &ObserverEvent) {
            *self.events.lock().unwrap() += 1;
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn name(&self) -> &str { "noop" }
        fn as_any(&self) -> &dyn std::any::Any { self }
    }

    fn all_on() -> ProgressEventToggles {
        ProgressEventToggles {
            agent_start: true, agent_end: true,
            tool_call_start: true, tool_call: true,
            llm_thinking: true, error: true,
        }
    }

    #[tokio::test]
    async fn emits_status_for_agent_start_and_passes_to_inner() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "exec-1".into(),
            mock.clone() as Arc<dyn Channel>,
            "u-1".into(),
            None,
            all_on(),
            inner.clone() as Arc<dyn Observer>,
        );

        obs.record_event(&ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "sonnet".into(),
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(mock.count(), 1, "one status_update should have fired");
        assert_eq!(inner.count(), 1, "inner observer must be called too");
        let recorded = mock.last().unwrap();
        assert_eq!(recorded.execution_id, "exec-1");
        assert_eq!(recorded.name, "agent");
    }

    #[tokio::test]
    async fn skips_emission_when_toggle_disabled_still_passes_through() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "exec-2".into(),
            mock.clone() as Arc<dyn Channel>,
            "u".into(),
            None,
            ProgressEventToggles::default(),
            inner.clone() as Arc<dyn Observer>,
        );

        obs.record_event(&ObserverEvent::AgentStart {
            provider: "p".into(), model: "m".into(),
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(mock.count(), 0, "no status_update should have fired");
        assert_eq!(inner.count(), 1, "inner observer must still be called");
    }

    #[tokio::test]
    async fn unrelated_events_still_pass_through_inner_with_no_emission() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "e".into(),
            mock.clone() as Arc<dyn Channel>,
            "u".into(),
            None,
            all_on(),
            inner.clone() as Arc<dyn Observer>,
        );

        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::CacheHit {
            cache_type: "hot".into(), tokens_saved: 50,
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(mock.count(), 0);
        assert_eq!(inner.count(), 2);
    }

    #[tokio::test]
    async fn record_metric_and_flush_passthrough() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "e".into(),
            mock.clone() as Arc<dyn Channel>,
            "u".into(),
            None,
            all_on(),
            inner.clone() as Arc<dyn Observer>,
        );
        obs.record_metric(&ObserverMetric::TokensUsed(10));
        obs.flush();
        assert_eq!(obs.name(), "progress-reporting");
    }
}
