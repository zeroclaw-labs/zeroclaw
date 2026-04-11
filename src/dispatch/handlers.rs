//! Standard `EventHandler` implementations.
//!
//! These cover the most common reactions to dispatch events:
//!
//! - [`NotificationHandler`] — sends a templated message via a configured
//!   `Channel` when an event matches a source/topic filter.
//! - [`AgentTriggerHandler`] — pushes the event as a `ChannelMessage` into
//!   an existing agent-loop input channel so the LLM agent can react.
//!
//! Both handlers are deliberately small and composable. Application code
//! constructs them with the right `Arc<dyn Channel>`, recipient, and topic
//! filter at startup, then registers them with the global `EventRouter`.

use std::sync::Arc;

use async_trait::async_trait;

use super::router::EventHandler;
use super::types::{DispatchEvent, EventSource, HandlerOutcome};
use crate::channels::traits::{Channel, ChannelMessage, SendMessage};

/// Optional source/topic filter for handler matching.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// If set, only events from this source match.
    pub source: Option<EventSource>,
    /// If set, only events whose `topic` starts with this prefix match.
    /// Matching is plain string prefix — no wildcards.
    pub topic_prefix: Option<String>,
}

impl EventFilter {
    pub fn any() -> Self {
        Self::default()
    }

    pub fn source(mut self, source: EventSource) -> Self {
        self.source = Some(source);
        self
    }

    pub fn topic_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.topic_prefix = Some(prefix.into());
        self
    }

    pub fn matches(&self, event: &DispatchEvent) -> bool {
        if let Some(s) = self.source {
            if event.source != s {
                return false;
            }
        }
        if let Some(ref prefix) = self.topic_prefix {
            match event.topic.as_deref() {
                Some(t) if t.starts_with(prefix.as_str()) => {}
                _ => return false,
            }
        }
        true
    }
}

/// Handler that sends a notification message via a `Channel` when an event matches.
///
/// The message body is a simple template — `{topic}` and `{payload}` are
/// substituted with the event's topic and payload (or empty strings if absent).
/// Anything more elaborate should use a custom handler.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use zeroclaw::dispatch::handlers::{EventFilter, NotificationHandler};
/// use zeroclaw::dispatch::EventSource;
///
/// let handler = NotificationHandler::new(
///     "doorbell_notifier",
///     kakao_channel.clone(),
///     "user_kakao_uid_123",
///     "🚪 Someone is at the door ({topic})",
/// )
/// .with_filter(
///     EventFilter::any()
///         .source(EventSource::Peripheral)
///         .topic_prefix("rpi-gpio/doorbell"),
/// );
/// router.register(Arc::new(handler));
/// ```
pub struct NotificationHandler {
    name: String,
    channel: Arc<dyn Channel>,
    recipient: String,
    template: String,
    filter: EventFilter,
}

impl NotificationHandler {
    pub fn new(
        name: impl Into<String>,
        channel: Arc<dyn Channel>,
        recipient: impl Into<String>,
        template: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            channel,
            recipient: recipient.into(),
            template: template.into(),
            filter: EventFilter::default(),
        }
    }

    pub fn with_filter(mut self, filter: EventFilter) -> Self {
        self.filter = filter;
        self
    }

    fn render(&self, event: &DispatchEvent) -> String {
        self.template
            .replace("{topic}", event.topic.as_deref().unwrap_or(""))
            .replace("{payload}", event.payload.as_deref().unwrap_or(""))
            .replace("{source}", &event.source.to_string())
    }
}

#[async_trait]
impl EventHandler for NotificationHandler {
    fn name(&self) -> &str {
        &self.name
    }

    fn matches(&self, event: &DispatchEvent) -> bool {
        self.filter.matches(event)
    }

    async fn handle(&self, event: &DispatchEvent) -> anyhow::Result<HandlerOutcome> {
        let body = self.render(event);
        let message = SendMessage::new(body.clone(), &self.recipient);
        self.channel.send(&message).await?;
        Ok(HandlerOutcome::Handled {
            summary: format!("notified {} via {}", self.recipient, self.channel.name()),
        })
    }
}

/// Handler that injects matching events into an agent loop's input channel.
///
/// Holds a clone of the `tokio::sync::mpsc::Sender<ChannelMessage>` that the
/// agent dispatcher is reading from. When an event matches, it builds a
/// synthetic `ChannelMessage` (with `silent = true` so the agent does not
/// surface it to a user) and `try_send`s it. Failures are returned as
/// `HandlerOutcome::Failed` so the dispatch audit log records them.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
/// use zeroclaw::dispatch::handlers::{AgentTriggerHandler, EventFilter};
/// use zeroclaw::dispatch::EventSource;
///
/// // The agent loop already exposes its input mpsc Sender at startup.
/// let handler = AgentTriggerHandler::new(
///     "agent_doorbell",
///     agent_input_tx.clone(),
///     "[peripheral] Doorbell rang — describe what the camera shows",
/// )
/// .with_filter(EventFilter::any().topic_prefix("rpi-gpio/doorbell"));
/// router.register(Arc::new(handler));
/// ```
pub struct AgentTriggerHandler {
    name: String,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    prompt: String,
    filter: EventFilter,
}

impl AgentTriggerHandler {
    pub fn new(
        name: impl Into<String>,
        tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        prompt: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            tx,
            prompt: prompt.into(),
            filter: EventFilter::default(),
        }
    }

    pub fn with_filter(mut self, filter: EventFilter) -> Self {
        self.filter = filter;
        self
    }

    fn build_channel_message(&self, event: &DispatchEvent) -> ChannelMessage {
        let content = format!(
            "{}\n\n[event] source={} topic={} payload={}",
            self.prompt,
            event.source,
            event.topic.as_deref().unwrap_or(""),
            event.payload.as_deref().unwrap_or(""),
        );
        ChannelMessage {
            id: uuid::Uuid::new_v4().to_string(),
            sender: format!("dispatch::{}", self.name),
            reply_target: format!("dispatch::{}", self.name),
            content,
            channel: "dispatch".to_string(),
            timestamp: now_secs(),
            thread_ts: None,
            silent: true, // dispatch-injected events should not pop user notifications
        }
    }
}

#[async_trait]
impl EventHandler for AgentTriggerHandler {
    fn name(&self) -> &str {
        &self.name
    }

    fn matches(&self, event: &DispatchEvent) -> bool {
        self.filter.matches(event)
    }

    async fn handle(&self, event: &DispatchEvent) -> anyhow::Result<HandlerOutcome> {
        let msg = self.build_channel_message(event);
        // try_send avoids blocking if the agent dispatcher is saturated;
        // dropped events are reported as Failed so the audit log surfaces them.
        match self.tx.try_send(msg) {
            Ok(_) => Ok(HandlerOutcome::Handled {
                summary: "agent input enqueued".into(),
            }),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(HandlerOutcome::Failed {
                error: "agent input channel full".into(),
            }),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Ok(HandlerOutcome::Failed {
                error: "agent input channel closed".into(),
            }),
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
    use parking_lot::Mutex;

    /// Mock channel that records every send().
    struct MockChannel {
        sent: Mutex<Vec<SendMessage>>,
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.lock().push(message.clone());
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn mock_channel() -> Arc<MockChannel> {
        Arc::new(MockChannel {
            sent: Mutex::new(Vec::new()),
        })
    }

    fn peripheral_event(topic: &str, payload: Option<&str>) -> DispatchEvent {
        DispatchEvent::new(
            EventSource::Peripheral,
            Some(topic.into()),
            payload.map(String::from),
        )
    }

    // ── EventFilter ──────────────────────────────────────────────

    #[test]
    fn filter_any_matches_everything() {
        let f = EventFilter::any();
        assert!(f.matches(&peripheral_event("nucleo/pin_3", Some("1"))));
        assert!(f.matches(&DispatchEvent::new(EventSource::Mqtt, None, None)));
    }

    #[test]
    fn filter_source_matches_only_that_source() {
        let f = EventFilter::any().source(EventSource::Peripheral);
        assert!(f.matches(&peripheral_event("nucleo/pin_3", None)));
        assert!(!f.matches(&DispatchEvent::new(EventSource::Mqtt, None, None)));
    }

    #[test]
    fn filter_topic_prefix_matches_only_matching_prefix() {
        let f = EventFilter::any().topic_prefix("rpi-gpio/doorbell");
        assert!(f.matches(&peripheral_event("rpi-gpio/doorbell", None)));
        assert!(f.matches(&peripheral_event("rpi-gpio/doorbell/front", None)));
        assert!(!f.matches(&peripheral_event("rpi-gpio/temp", None)));
        assert!(!f.matches(&DispatchEvent::new(EventSource::Mqtt, None, None)));
    }

    #[test]
    fn filter_combined_source_and_prefix() {
        let f = EventFilter::any()
            .source(EventSource::Peripheral)
            .topic_prefix("nucleo/");
        assert!(f.matches(&peripheral_event("nucleo/pin_3", None)));
        assert!(!f.matches(&peripheral_event("rpi-gpio/pin_3", None)));
    }

    // ── NotificationHandler ──────────────────────────────────────

    #[tokio::test]
    async fn notification_handler_sends_via_channel() {
        let channel = mock_channel();
        let handler = NotificationHandler::new(
            "doorbell",
            channel.clone(),
            "user_123",
            "Doorbell at {topic}",
        );

        let event = peripheral_event("rpi-gpio/doorbell", Some("1"));
        let outcome = handler.handle(&event).await.unwrap();

        match outcome {
            HandlerOutcome::Handled { .. } => {}
            other => panic!("expected Handled, got {other:?}"),
        }

        let sent = channel.sent.lock();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].recipient, "user_123");
        assert_eq!(sent[0].content, "Doorbell at rpi-gpio/doorbell");
    }

    #[tokio::test]
    async fn notification_handler_template_substitutes_payload_and_source() {
        let channel = mock_channel();
        let handler = NotificationHandler::new(
            "alarm",
            channel.clone(),
            "ops",
            "[{source}] {topic} = {payload}",
        );

        let event = peripheral_event("nucleo/temp", Some("87.3"));
        handler.handle(&event).await.unwrap();

        assert_eq!(
            channel.sent.lock()[0].content,
            "[peripheral] nucleo/temp = 87.3"
        );
    }

    #[tokio::test]
    async fn notification_handler_respects_filter() {
        let channel = mock_channel();
        let handler = NotificationHandler::new("doorbell", channel.clone(), "u1", "ring")
            .with_filter(EventFilter::any().topic_prefix("rpi-gpio/doorbell"));

        assert!(handler.matches(&peripheral_event("rpi-gpio/doorbell", None)));
        assert!(!handler.matches(&peripheral_event("rpi-gpio/temp", None)));
    }

    // ── AgentTriggerHandler ──────────────────────────────────────

    #[tokio::test]
    async fn agent_trigger_handler_enqueues_message() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let handler = AgentTriggerHandler::new(
            "agent_doorbell",
            tx,
            "Describe what the camera shows",
        );

        let event = peripheral_event("rpi-gpio/doorbell", Some("1"));
        let outcome = handler.handle(&event).await.unwrap();
        assert!(matches!(outcome, HandlerOutcome::Handled { .. }));

        let received = rx.recv().await.unwrap();
        assert!(received.silent, "dispatch-injected events must be silent");
        assert!(received.content.contains("Describe what the camera shows"));
        assert!(received.content.contains("topic=rpi-gpio/doorbell"));
        assert!(received.content.contains("payload=1"));
        assert_eq!(received.channel, "dispatch");
    }

    #[tokio::test]
    async fn agent_trigger_handler_reports_full_channel_as_failed() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        // Fill the channel.
        tx.send(ChannelMessage {
            id: "filler".into(),
            sender: "x".into(),
            reply_target: "x".into(),
            content: "filler".into(),
            channel: "test".into(),
            timestamp: 0,
            thread_ts: None,
            silent: false,
        })
        .await
        .unwrap();

        let handler = AgentTriggerHandler::new("agent", tx, "trigger");
        let event = peripheral_event("any", None);
        let outcome = handler.handle(&event).await.unwrap();
        assert!(matches!(outcome, HandlerOutcome::Failed { .. }));
    }

    #[tokio::test]
    async fn agent_trigger_handler_reports_closed_channel_as_failed() {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        drop(rx); // close the channel

        let handler = AgentTriggerHandler::new("agent", tx, "trigger");
        let event = peripheral_event("any", None);
        let outcome = handler.handle(&event).await.unwrap();
        match outcome {
            HandlerOutcome::Failed { error } => assert!(error.contains("closed")),
            other => panic!("expected Failed(closed), got {other:?}"),
        }
    }
}
