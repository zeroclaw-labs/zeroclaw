//! Forwarding of a foreground delegate's tool calls to the parent turn.
//!
//! A foreground agentic delegate runs its own tool loop. That loop reports
//! its tool calls to a [`DelegateActivityForwarder`], which maps each start
//! and finish into [`TurnEvent::DelegateActivity`] on the parent turn's event
//! sender, so a client can show what the child is doing while the parent
//! waits. Only the tool name, a secret-redacted argument summary and a
//! bounded output preview leave the child; its text, reasoning, usage and
//! approvals do not.

use crate::agent::tool_execution::ParentTurnEvents;
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use parking_lot::Mutex;
use zeroclaw_api::agent::{DelegateActivity, DelegateActivityPhase, TurnEvent};

/// Characters kept of a child tool's output.
pub(crate) const OUTPUT_PREVIEW_CHARS: usize = 400;
/// Characters kept of a child tool's name.
const TOOL_NAME_CHARS: usize = 64;
/// Id-less calls tracked as in flight at once; the oldest is dropped beyond this.
const PENDING_UNNAMED_LIMIT: usize = 32;

/// The sink a delegate the child runs should report to: the same parent
/// delegate call, one level deeper.
pub(crate) fn one_level_deeper(parent: ParentTurnEvents) -> ParentTurnEvents {
    ParentTurnEvents {
        depth: parent.depth.saturating_add(1),
        ..parent
    }
}

/// Observer for a foreground delegate's tool loop that relays its tool calls
/// to the parent turn.
///
/// Observer callbacks are synchronous, so the relay uses `try_send`: a full
/// parent channel drops the activity rather than slowing the child or the
/// parent turn.
pub(crate) struct DelegateActivityForwarder {
    parent: ParentTurnEvents,
    agent: String,
    /// Ids handed to text-protocol calls, which carry no provider id, so a
    /// finish matches its start. Matched by tool name and original arguments,
    /// oldest first when identical calls overlap.
    pending_unnamed: Mutex<Vec<(String, Option<String>, String)>>,
}

impl DelegateActivityForwarder {
    pub(crate) fn new(parent: ParentTurnEvents, agent: &str) -> Self {
        Self {
            parent,
            agent: agent.to_string(),
            pending_unnamed: Mutex::new(Vec::new()),
        }
    }

    fn activity_for(&self, event: &ObserverEvent) -> Option<DelegateActivity> {
        let (tool_call_id, tool, phase) = match event {
            ObserverEvent::ToolCallStart {
                tool,
                tool_call_id,
                arguments,
                ..
            } => {
                let id = match tool_call_id {
                    Some(id) => id.clone(),
                    None => {
                        let id = uuid::Uuid::new_v4().to_string();
                        let mut pending = self.pending_unnamed.lock();
                        pending.push((tool.clone(), arguments.clone(), id.clone()));
                        let excess = pending.len().saturating_sub(PENDING_UNNAMED_LIMIT);
                        pending.drain(..excess);
                        id
                    }
                };
                let mut arguments = arguments
                    .as_deref()
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or(serde_json::Value::Null);
                // A tool's own `approved` input is not an approval decision;
                // shown under the card it reads as a refusal.
                if let Some(map) = arguments.as_object_mut() {
                    map.remove("approved");
                }
                // The summary redacts only top-level keys; scrub nested
                // credential keys and inline secrets first.
                let arguments = crate::agent::turn::redact::scrub_credentials_value(arguments);
                (
                    id,
                    tool,
                    DelegateActivityPhase::Started {
                        arguments_summary: crate::approval::summarize_args(&arguments),
                    },
                )
            }
            ObserverEvent::ToolCall {
                tool,
                tool_call_id,
                arguments,
                success,
                result,
                ..
            } => {
                let id = match tool_call_id {
                    Some(id) => id.clone(),
                    None => {
                        let mut pending = self.pending_unnamed.lock();
                        let at = pending
                            .iter()
                            .position(|(name, args, _)| name == tool && args == arguments)?;
                        pending.remove(at).2
                    }
                };
                // The loop scrubs credentials from `result` before it records it.
                let output = result.as_deref().unwrap_or_default();
                (
                    id,
                    tool,
                    DelegateActivityPhase::Finished {
                        success: *success,
                        output_preview: crate::util::truncate_with_ellipsis(
                            output,
                            OUTPUT_PREVIEW_CHARS,
                        ),
                    },
                )
            }
            _ => return None,
        };
        Some(DelegateActivity {
            parent_tool_call_id: self.parent.parent_tool_call_id.clone(),
            agent: self.agent.clone(),
            depth: self.parent.depth,
            tool_call_id,
            tool_name: tool.chars().take(TOOL_NAME_CHARS).collect(),
            phase,
        })
    }
}

impl Observer for DelegateActivityForwarder {
    fn record_event(&self, event: &ObserverEvent) {
        if let Some(activity) = self.activity_for(event) {
            let _ = self
                .parent
                .tx
                .try_send(TurnEvent::DelegateActivity(activity));
        }
    }

    fn record_metric(&self, _metric: &ObserverMetric) {}

    fn name(&self) -> &str {
        "delegate-activity"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn forwarder(capacity: usize) -> (DelegateActivityForwarder, mpsc::Receiver<TurnEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        let parent = ParentTurnEvents {
            tx,
            parent_tool_call_id: "parent-call".into(),
            depth: 0,
        };
        (DelegateActivityForwarder::new(parent, "researcher"), rx)
    }

    fn start(id: Option<&str>, tool: &str, arguments: &str) -> ObserverEvent {
        ObserverEvent::ToolCallStart {
            tool: tool.into(),
            tool_call_id: id.map(Into::into),
            arguments: Some(arguments.into()),
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        }
    }

    fn finish(
        id: Option<&str>,
        tool: &str,
        arguments: &str,
        result: &str,
        success: bool,
    ) -> ObserverEvent {
        ObserverEvent::ToolCall {
            tool: tool.into(),
            tool_call_id: id.map(Into::into),
            duration: Duration::ZERO,
            success,
            arguments: Some(arguments.into()),
            result: Some(result.into()),
            channel: None,
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: None,
        }
    }

    fn next(rx: &mut mpsc::Receiver<TurnEvent>) -> DelegateActivity {
        match rx.try_recv() {
            Ok(TurnEvent::DelegateActivity(activity)) => activity,
            other => panic!("expected delegate activity, got {other:?}"),
        }
    }

    #[test]
    fn start_carries_a_redacted_summary_and_finish_a_bounded_preview() {
        let (fwd, mut rx) = forwarder(8);
        fwd.record_event(&start(
            Some("c1"),
            "http_request",
            r#"{"url":"https://example.com","api_key":"sk-live-SECRET"}"#,
        ));
        fwd.record_event(&finish(
            Some("c1"),
            "http_request",
            r#"{"url":"https://example.com","api_key":"sk-live-SECRET"}"#,
            &"x".repeat(10_000),
            true,
        ));

        let started = next(&mut rx);
        assert_eq!(started.parent_tool_call_id, "parent-call");
        assert_eq!(started.agent, "researcher");
        assert_eq!(started.tool_call_id, "c1");
        let DelegateActivityPhase::Started { arguments_summary } = started.phase else {
            panic!("expected a start");
        };
        assert!(arguments_summary.contains("https://example.com"));
        assert!(!arguments_summary.contains("SECRET"), "{arguments_summary}");

        let finished = next(&mut rx);
        assert_eq!(finished.tool_call_id, "c1");
        let DelegateActivityPhase::Finished { output_preview, .. } = finished.phase else {
            panic!("expected a finish");
        };
        assert!(output_preview.chars().count() <= OUTPUT_PREVIEW_CHARS + 3);
    }

    #[test]
    fn nested_and_inline_credentials_never_reach_the_summary() {
        let (fwd, mut rx) = forwarder(8);
        fwd.record_event(&start(
            Some("c1"),
            "http_request",
            r#"{"url":"https://example.com","headers":{"Authorization":"Bearer sk-live-NESTED"},"body":"token=sk-live-INLINE"}"#,
        ));
        let DelegateActivityPhase::Started { arguments_summary } = next(&mut rx).phase else {
            panic!("expected a start");
        };
        assert!(arguments_summary.contains("https://example.com"));
        assert!(!arguments_summary.contains("NESTED"), "{arguments_summary}");
        assert!(!arguments_summary.contains("INLINE"), "{arguments_summary}");
    }

    #[test]
    fn id_less_calls_get_one_id_shared_by_start_and_finish() {
        let (fwd, mut rx) = forwarder(8);
        fwd.record_event(&start(None, "shell", "{}"));
        fwd.record_event(&finish(None, "shell", "{}", "ok", true));
        let started = next(&mut rx);
        let finished = next(&mut rx);
        assert!(!started.tool_call_id.is_empty());
        assert_eq!(started.tool_call_id, finished.tool_call_id);
    }

    #[test]
    fn id_less_parallel_calls_keep_their_results_when_finishes_arrive_in_reverse_order() {
        let (fwd, mut rx) = forwarder(8);
        let first_args = r#"{"command":"echo first"}"#;
        let second_args = r#"{"command":"echo second"}"#;
        fwd.record_event(&start(None, "shell", first_args));
        fwd.record_event(&start(None, "shell", second_args));
        fwd.record_event(&finish(None, "shell", second_args, "failed second", false));
        fwd.record_event(&finish(None, "shell", first_args, "first", true));

        let first_start = next(&mut rx);
        let second_start = next(&mut rx);
        let second_finish = next(&mut rx);
        let first_finish = next(&mut rx);
        assert_ne!(first_start.tool_call_id, second_start.tool_call_id);
        assert_eq!(first_start.tool_call_id, first_finish.tool_call_id);
        assert_eq!(second_start.tool_call_id, second_finish.tool_call_id);
        assert_eq!(
            first_finish.phase,
            DelegateActivityPhase::Finished {
                success: true,
                output_preview: "first".into(),
            }
        );
        assert_eq!(
            second_finish.phase,
            DelegateActivityPhase::Finished {
                success: false,
                output_preview: "failed second".into(),
            }
        );
    }

    #[test]
    fn a_full_parent_channel_drops_activity_without_blocking() {
        let (fwd, mut rx) = forwarder(1);
        for i in 0..5 {
            fwd.record_event(&start(Some(&format!("c{i}")), "shell", "{}"));
        }
        let mut received = 0;
        while rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, 1, "only what fits in the parent channel arrives");
    }
}
