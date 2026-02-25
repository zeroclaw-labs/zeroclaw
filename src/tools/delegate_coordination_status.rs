use super::traits::{Tool, ToolResult};
use crate::coordination::{CoordinationPayload, InMemoryMessageBus, SequencedEnvelope};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

const DEFAULT_DEAD_LETTER_LIMIT: usize = 10;
const MAX_DEAD_LETTER_LIMIT: usize = 100;
const MAX_DEAD_LETTER_OFFSET: usize = 10_000;
const DEFAULT_MESSAGE_LIMIT: usize = 5;
const MAX_MESSAGE_LIMIT: usize = 50;
const MAX_MESSAGE_OFFSET: usize = 10_000;
const DEFAULT_CONTEXT_LIMIT: usize = 25;
const MAX_CONTEXT_LIMIT: usize = 200;
const MAX_CONTEXT_OFFSET: usize = 10_000;

/// Read-only runtime observability tool for delegate coordination events.
pub struct DelegateCoordinationStatusTool {
    bus: InMemoryMessageBus,
    security: Arc<SecurityPolicy>,
}

impl DelegateCoordinationStatusTool {
    pub fn new(bus: InMemoryMessageBus, security: Arc<SecurityPolicy>) -> Self {
        Self { bus, security }
    }
}

#[async_trait]
impl Tool for DelegateCoordinationStatusTool {
    fn name(&self) -> &str {
        "delegate_coordination_status"
    }

    fn description(&self) -> &str {
        "Inspect delegate coordination runtime state (agent inbox backlog, context state transitions, and dead-letter events)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Optional agent name. If set, only that inbox is reported."
                },
                "correlation_id": {
                    "type": "string",
                    "description": "Optional delegation correlation ID. Filters context/dead-letter output."
                },
                "include_messages": {
                    "type": "boolean",
                    "description": "Include peeked message previews for inboxes",
                    "default": false
                },
                "message_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_MESSAGE_LIMIT,
                    "description": "Max number of preview messages per inbox when include_messages=true",
                    "default": DEFAULT_MESSAGE_LIMIT
                },
                "message_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_MESSAGE_OFFSET,
                    "description": "Offset into preview messages ordered by oldest first (or matching oldest first when correlation_id is set)",
                    "default": 0
                },
                "include_dead_letters": {
                    "type": "boolean",
                    "description": "Include dead-letter preview entries",
                    "default": true
                },
                "dead_letter_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_DEAD_LETTER_LIMIT,
                    "description": "Max number of dead-letter entries to include",
                    "default": DEFAULT_DEAD_LETTER_LIMIT
                },
                "dead_letter_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_DEAD_LETTER_OFFSET,
                    "description": "Offset into dead-letter entries ordered by newest first",
                    "default": 0
                },
                "context_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_CONTEXT_LIMIT,
                    "description": "Max number of context entries to include (ordered by newest update first)",
                    "default": DEFAULT_CONTEXT_LIMIT
                },
                "context_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_CONTEXT_OFFSET,
                    "description": "Offset into context entries ordered by newest update first",
                    "default": 0
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, self.name())
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let filter_agent = args
            .get("agent")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let filter_correlation = args
            .get("correlation_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let include_messages = args
            .get("include_messages")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let message_limit = clamp_usize(
            args.get("message_limit")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            DEFAULT_MESSAGE_LIMIT,
            MAX_MESSAGE_LIMIT,
        );
        let message_offset = clamp_offset(
            args.get("message_offset")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            MAX_MESSAGE_OFFSET,
        );
        let include_dead_letters = args
            .get("include_dead_letters")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let dead_letter_limit = clamp_usize(
            args.get("dead_letter_limit")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            DEFAULT_DEAD_LETTER_LIMIT,
            MAX_DEAD_LETTER_LIMIT,
        );
        let dead_letter_offset = clamp_offset(
            args.get("dead_letter_offset")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            MAX_DEAD_LETTER_OFFSET,
        );
        let context_limit = clamp_usize(
            args.get("context_limit")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            DEFAULT_CONTEXT_LIMIT,
            MAX_CONTEXT_LIMIT,
        );
        let context_offset = clamp_offset(
            args.get("context_offset")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            MAX_CONTEXT_OFFSET,
        );

        let agents = if let Some(agent) = filter_agent.clone() {
            vec![agent]
        } else {
            self.bus.registered_agents()
        };

        let mut inboxes = Vec::new();
        for agent in agents {
            let pending = match self.bus.pending_for_agent(&agent) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let pending_filtered = filter_correlation.as_deref().and_then(|correlation_id| {
                self.bus
                    .pending_for_agent_correlation(&agent, correlation_id)
                    .ok()
            });

            let mut message_total = 0usize;
            let mut message_preview = Vec::new();
            if include_messages {
                let matched_messages = if let Some(correlation_id) = filter_correlation.as_deref() {
                    message_total = pending_filtered.unwrap_or(0);
                    self.bus
                        .peek_for_agent_correlation_with_offset(
                            &agent,
                            correlation_id,
                            message_offset,
                            message_limit,
                        )
                        .unwrap_or_default()
                } else {
                    message_total = pending;
                    self.bus
                        .peek_for_agent_with_offset(&agent, message_offset, message_limit)
                        .unwrap_or_default()
                };

                message_preview = matched_messages
                    .into_iter()
                    .map(summarize_envelope)
                    .collect::<Vec<_>>();
            }
            let messages_returned = message_preview.len();
            let messages_truncated = include_messages
                && message_offset.saturating_add(messages_returned) < message_total;
            let message_next_offset = (include_messages && messages_truncated)
                .then_some(message_offset + messages_returned);

            inboxes.push(json!({
                "agent": agent,
                "pending": pending,
                "pending_filtered": pending_filtered,
                "message_total": message_total,
                "message_offset": message_offset,
                "messages_returned": messages_returned,
                "messages_truncated": messages_truncated,
                "message_next_offset": message_next_offset,
                "messages": message_preview
            }));
        }

        let (contexts_total, context_entries) =
            if let Some(correlation_id) = filter_correlation.as_deref() {
                (
                    self.bus
                        .delegate_context_count_for_correlation(correlation_id),
                    self.bus
                        .delegate_context_entries_recent_for_correlation_with_offset(
                            correlation_id,
                            context_offset,
                            context_limit,
                        ),
                )
            } else {
                (
                    self.bus.delegate_context_count(),
                    self.bus
                        .delegate_context_entries_recent_with_offset(context_offset, context_limit),
                )
            };
        let contexts = context_entries
            .into_iter()
            .map(|(key, entry)| {
                json!({
                    "key": key,
                    "version": entry.version,
                    "updated_by": entry.updated_by,
                    "last_message_id": entry.last_message_id,
                    "value": entry.value
                })
            })
            .collect::<Vec<_>>();
        let contexts_returned = contexts.len();
        let contexts_truncated = context_offset.saturating_add(contexts_returned) < contexts_total;
        let context_next_offset = contexts_truncated.then_some(context_offset + contexts_returned);

        let mut dead_letter_preview = Vec::new();
        let mut dead_letters_total = 0usize;
        if include_dead_letters {
            let matching = if let Some(correlation_id) = filter_correlation.as_deref() {
                dead_letters_total = self.bus.dead_letter_count_for_correlation(correlation_id);
                self.bus.dead_letters_recent_for_correlation(
                    correlation_id,
                    dead_letter_offset,
                    dead_letter_limit,
                )
            } else {
                dead_letters_total = self.bus.dead_letter_count();
                self.bus
                    .dead_letters_recent(dead_letter_offset, dead_letter_limit)
            };
            dead_letter_preview = matching
                .into_iter()
                .rev()
                .map(|entry| {
                    json!({
                        "message_id": entry.envelope.id,
                        "topic": entry.envelope.topic,
                        "from": entry.envelope.from,
                        "to": entry.envelope.to,
                        "correlation_id": entry.envelope.correlation_id,
                        "payload_kind": payload_kind(&entry.envelope.payload),
                        "reason": entry.reason
                    })
                })
                .collect::<Vec<_>>();
        }
        let dead_letters_returned = dead_letter_preview.len();
        let dead_letters_truncated =
            dead_letter_offset.saturating_add(dead_letters_returned) < dead_letters_total;
        let dead_letter_next_offset =
            dead_letters_truncated.then_some(dead_letter_offset + dead_letters_returned);

        let delegate_context_count_filtered = filter_correlation
            .as_deref()
            .map(|correlation_id| {
                self.bus
                    .delegate_context_count_for_correlation(correlation_id)
            })
            .unwrap_or_else(|| self.bus.delegate_context_count());

        let output = json!({
            "subscriber_count": self.bus.subscriber_count(),
            "context_count": self.bus.context_count(),
            "delegate_context_count": self.bus.delegate_context_count(),
            "delegate_context_count_filtered": delegate_context_count_filtered,
            "dead_letter_count": self.bus.dead_letter_count(),
            "limits": self.bus.limits(),
            "stats": self.bus.stats(),
            "filter": {
                "agent": filter_agent,
                "correlation_id": filter_correlation
            },
            "contexts_total": contexts_total,
            "contexts_offset": context_offset,
            "contexts_returned": contexts_returned,
            "contexts_truncated": contexts_truncated,
            "context_next_offset": context_next_offset,
            "dead_letters_total": dead_letters_total,
            "dead_letter_offset": dead_letter_offset,
            "dead_letters_returned": dead_letters_returned,
            "dead_letters_truncated": dead_letters_truncated,
            "dead_letter_next_offset": dead_letter_next_offset,
            "inboxes": inboxes,
            "contexts": contexts,
            "dead_letters": dead_letter_preview
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_default(),
            error: None,
        })
    }
}

fn clamp_usize(value: Option<usize>, default_value: usize, max_value: usize) -> usize {
    match value {
        Some(value) if value > 0 => value.min(max_value),
        _ => default_value,
    }
}

fn clamp_offset(value: Option<usize>, max_value: usize) -> usize {
    value.unwrap_or(0).min(max_value)
}

fn summarize_envelope(entry: SequencedEnvelope) -> serde_json::Value {
    json!({
        "sequence": entry.sequence,
        "message_id": entry.envelope.id,
        "topic": entry.envelope.topic,
        "from": entry.envelope.from,
        "to": entry.envelope.to,
        "correlation_id": entry.envelope.correlation_id,
        "causation_id": entry.envelope.causation_id,
        "payload_kind": payload_kind(&entry.envelope.payload)
    })
}

fn payload_kind(payload: &CoordinationPayload) -> &'static str {
    match payload {
        CoordinationPayload::DelegateTask { .. } => "delegate_task",
        CoordinationPayload::ContextPatch { .. } => "context_patch",
        CoordinationPayload::TaskResult { .. } => "task_result",
        CoordinationPayload::Ack { .. } => "ack",
        CoordinationPayload::Control { .. } => "control",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{CoordinationEnvelope, CoordinationPayload};

    fn test_bus() -> InMemoryMessageBus {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("delegate-lead")
            .expect("register lead should succeed");
        bus.register_agent("researcher")
            .expect("register researcher should succeed");
        bus
    }

    #[tokio::test]
    async fn status_tool_reports_context_and_inboxes() {
        let bus = test_bus();
        let mut request = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "researcher",
            "delegate:corr-1",
            "delegate.request",
            CoordinationPayload::DelegateTask {
                task_id: "corr-1".to_string(),
                summary: "Investigate".to_string(),
                metadata: json!({"priority":"high"}),
            },
        );
        request.correlation_id = Some("corr-1".to_string());
        bus.publish(request).expect("request should publish");

        let mut patch = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-1",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-1/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch.correlation_id = Some("corr-1".to_string());
        bus.publish(patch).expect("state patch should publish");

        let tool = DelegateCoordinationStatusTool::new(bus, Arc::new(SecurityPolicy::default()));
        let result = tool
            .execute(json!({
                "include_messages": true,
                "agent": "researcher",
                "correlation_id": "corr-1"
            }))
            .await
            .expect("tool execution should succeed");

        assert!(result.success);
        let parsed: serde_json::Value =
            serde_json::from_str(&result.output).expect("output must be valid JSON");
        assert_eq!(parsed["inboxes"].as_array().map(Vec::len), Some(1));
        assert_eq!(parsed["context_count"], json!(1));
        assert_eq!(parsed["delegate_context_count"], json!(1));
        assert_eq!(parsed["delegate_context_count_filtered"], json!(1));
        assert_eq!(parsed["contexts_total"], json!(1));
        assert_eq!(parsed["contexts_offset"], json!(0));
        assert_eq!(parsed["contexts_returned"], json!(1));
        assert_eq!(parsed["contexts_truncated"], json!(false));
        assert_eq!(parsed["context_next_offset"], serde_json::Value::Null);
        assert_eq!(parsed["contexts"].as_array().map(Vec::len), Some(1));
        assert_eq!(parsed["inboxes"][0]["pending"], json!(1));
        assert_eq!(parsed["inboxes"][0]["pending_filtered"], json!(1));
        assert_eq!(parsed["inboxes"][0]["message_total"], json!(1));
        assert_eq!(parsed["inboxes"][0]["message_offset"], json!(0));
        assert_eq!(parsed["inboxes"][0]["messages_returned"], json!(1));
        assert_eq!(parsed["inboxes"][0]["messages_truncated"], json!(false));
        assert_eq!(
            parsed["inboxes"][0]["message_next_offset"],
            serde_json::Value::Null
        );
        assert_eq!(parsed["dead_letters_total"], json!(0));
        assert_eq!(parsed["dead_letters_returned"], json!(0));
        assert_eq!(parsed["dead_letters_truncated"], json!(false));
        assert_eq!(parsed["dead_letter_next_offset"], serde_json::Value::Null);
        assert_eq!(parsed["limits"]["max_inbox_messages_per_agent"], json!(256));
        assert_eq!(parsed["limits"]["max_dead_letters"], json!(256));
        assert_eq!(parsed["limits"]["max_context_entries"], json!(512));
        assert_eq!(parsed["limits"]["max_seen_message_ids"], json!(4096));
        assert_eq!(parsed["stats"]["publish_attempts_total"], json!(2));
        assert_eq!(parsed["stats"]["deliveries_total"], json!(2));
        assert_eq!(parsed["stats"]["dead_letters_total"], json!(0));
        assert_eq!(parsed["stats"]["dead_letter_evictions_total"], json!(0));
        assert_eq!(parsed["stats"]["context_evictions_total"], json!(0));
        assert_eq!(parsed["stats"]["seen_message_id_evictions_total"], json!(0));
    }

    #[tokio::test]
    async fn status_tool_applies_dead_letter_limit() {
        let bus = test_bus();

        for index in 0..3 {
            let mut invalid = CoordinationEnvelope::new_direct(
                "delegate-lead",
                "researcher",
                format!("delegate:corr-{index}"),
                "delegate.result",
                CoordinationPayload::TaskResult {
                    task_id: format!("corr-{index}"),
                    success: false,
                    output: "failure".to_string(),
                },
            );
            invalid.id = format!("invalid-{index}");
            // Missing correlation id causes dead-letter.
            let _ = bus.publish(invalid);
        }

        let tool = DelegateCoordinationStatusTool::new(bus, Arc::new(SecurityPolicy::default()));
        let result = tool
            .execute(json!({
                "dead_letter_limit": 2
            }))
            .await
            .expect("tool execution should succeed");

        assert!(result.success);
        let parsed: serde_json::Value =
            serde_json::from_str(&result.output).expect("output must be valid JSON");
        assert_eq!(parsed["dead_letter_count"], json!(3));
        assert_eq!(parsed["contexts_total"], json!(0));
        assert_eq!(parsed["contexts_offset"], json!(0));
        assert_eq!(parsed["contexts_returned"], json!(0));
        assert_eq!(parsed["contexts_truncated"], json!(false));
        assert_eq!(parsed["context_next_offset"], serde_json::Value::Null);
        assert_eq!(parsed["dead_letters_total"], json!(3));
        assert_eq!(parsed["dead_letter_offset"], json!(0));
        assert_eq!(parsed["dead_letters_returned"], json!(2));
        assert_eq!(parsed["dead_letters_truncated"], json!(true));
        assert_eq!(parsed["dead_letter_next_offset"], json!(2));
        assert_eq!(parsed["dead_letters"].as_array().map(Vec::len), Some(2));
        assert_eq!(parsed["stats"]["publish_attempts_total"], json!(0));
        assert_eq!(parsed["stats"]["deliveries_total"], json!(0));
        assert_eq!(parsed["stats"]["dead_letters_total"], json!(3));
        assert_eq!(parsed["stats"]["context_evictions_total"], json!(0));
        assert_eq!(parsed["stats"]["seen_message_id_evictions_total"], json!(0));
    }

    #[tokio::test]
    async fn status_tool_applies_context_limit_in_recent_order() {
        let bus = test_bus();

        let mut patch_a = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-a",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_a.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a).expect("patch a0 should publish");

        let mut patch_b = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-b",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-b/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_b.correlation_id = Some("corr-b".to_string());
        bus.publish(patch_b).expect("patch b0 should publish");

        let mut patch_a_update = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-a",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 1,
                value: json!({"phase":"running"}),
            },
        );
        patch_a_update.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_update)
            .expect("patch a1 should publish");

        let mut patch_c = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-c",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-c/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_c.correlation_id = Some("corr-c".to_string());
        bus.publish(patch_c).expect("patch c0 should publish");

        let tool = DelegateCoordinationStatusTool::new(bus, Arc::new(SecurityPolicy::default()));
        let result = tool
            .execute(json!({
                "context_limit": 2,
                "include_dead_letters": false
            }))
            .await
            .expect("tool execution should succeed");

        assert!(result.success);
        let parsed: serde_json::Value =
            serde_json::from_str(&result.output).expect("output must be valid JSON");
        assert_eq!(parsed["context_count"], json!(3));
        assert_eq!(parsed["contexts_total"], json!(3));
        assert_eq!(parsed["contexts_offset"], json!(0));
        assert_eq!(parsed["contexts_returned"], json!(2));
        assert_eq!(parsed["contexts_truncated"], json!(true));
        assert_eq!(parsed["context_next_offset"], json!(2));
        assert_eq!(parsed["dead_letters_total"], json!(0));
        assert_eq!(parsed["dead_letters_returned"], json!(0));
        assert_eq!(parsed["dead_letters_truncated"], json!(false));
        assert_eq!(parsed["dead_letter_next_offset"], serde_json::Value::Null);
        assert_eq!(parsed["contexts"].as_array().map(Vec::len), Some(2));
        assert_eq!(parsed["contexts"][0]["key"], json!("delegate/corr-c/state"));
        assert_eq!(parsed["contexts"][1]["key"], json!("delegate/corr-a/state"));

        let second_page = tool
            .execute(json!({
                "context_limit": 2,
                "context_offset": 1,
                "include_dead_letters": false
            }))
            .await
            .expect("tool execution should succeed");
        assert!(second_page.success);
        let second_parsed: serde_json::Value =
            serde_json::from_str(&second_page.output).expect("output must be valid JSON");
        assert_eq!(second_parsed["contexts_total"], json!(3));
        assert_eq!(second_parsed["contexts_offset"], json!(1));
        assert_eq!(second_parsed["contexts_returned"], json!(2));
        assert_eq!(second_parsed["contexts_truncated"], json!(false));
        assert_eq!(
            second_parsed["context_next_offset"],
            serde_json::Value::Null
        );
        assert_eq!(
            second_parsed["contexts"][0]["key"],
            json!("delegate/corr-a/state")
        );
        assert_eq!(
            second_parsed["contexts"][1]["key"],
            json!("delegate/corr-b/state")
        );
    }

    #[tokio::test]
    async fn status_tool_applies_context_paging_with_correlation_filter() {
        let bus = test_bus();

        let mut patch_a_state = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-a",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_a_state.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_state)
            .expect("corr-a state patch should publish");

        let mut patch_b_state = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-b",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-b/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_b_state.correlation_id = Some("corr-b".to_string());
        bus.publish(patch_b_state)
            .expect("corr-b state patch should publish");

        let mut patch_a_output = CoordinationEnvelope::new_direct(
            "delegate-lead",
            "delegate-lead",
            "delegate:corr-a",
            "delegate.output",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/output".to_string(),
                expected_version: 0,
                value: json!({"summary":"ready"}),
            },
        );
        patch_a_output.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_output)
            .expect("corr-a output patch should publish");

        let tool = DelegateCoordinationStatusTool::new(bus, Arc::new(SecurityPolicy::default()));
        let result = tool
            .execute(json!({
                "correlation_id": "corr-a",
                "context_limit": 1,
                "context_offset": 1,
                "include_dead_letters": false
            }))
            .await
            .expect("tool execution should succeed");

        assert!(result.success);
        let parsed: serde_json::Value =
            serde_json::from_str(&result.output).expect("output must be valid JSON");
        assert_eq!(parsed["contexts_total"], json!(2));
        assert_eq!(parsed["contexts_offset"], json!(1));
        assert_eq!(parsed["contexts_returned"], json!(1));
        assert_eq!(parsed["contexts_truncated"], json!(false));
        assert_eq!(parsed["context_next_offset"], serde_json::Value::Null);
        assert_eq!(parsed["contexts"][0]["key"], json!("delegate/corr-a/state"));
    }

    #[tokio::test]
    async fn status_tool_applies_dead_letter_paging_with_correlation_filter() {
        let bus = test_bus();

        for (index, correlation_id) in [("0", "corr-a"), ("1", "corr-b"), ("2", "corr-a")] {
            let mut invalid = CoordinationEnvelope::new_direct(
                "delegate-lead",
                "unknown-agent",
                format!("delegate:{correlation_id}"),
                "delegate.request",
                CoordinationPayload::DelegateTask {
                    task_id: format!("task-{index}"),
                    summary: "invalid target".to_string(),
                    metadata: json!({}),
                },
            );
            invalid.id = format!("dead-corr-{index}");
            invalid.correlation_id = Some(correlation_id.to_string());
            let _ = bus.publish(invalid);
        }

        let tool = DelegateCoordinationStatusTool::new(bus, Arc::new(SecurityPolicy::default()));

        let first_page = tool
            .execute(json!({
                "correlation_id": "corr-a",
                "dead_letter_limit": 1,
                "dead_letter_offset": 0
            }))
            .await
            .expect("tool execution should succeed");
        assert!(first_page.success);
        let first_parsed: serde_json::Value =
            serde_json::from_str(&first_page.output).expect("output must be valid JSON");
        assert_eq!(first_parsed["dead_letters_total"], json!(2));
        assert_eq!(first_parsed["dead_letter_offset"], json!(0));
        assert_eq!(first_parsed["dead_letters_returned"], json!(1));
        assert_eq!(first_parsed["dead_letters_truncated"], json!(true));
        assert_eq!(first_parsed["dead_letter_next_offset"], json!(1));
        assert_eq!(
            first_parsed["dead_letters"][0]["message_id"],
            json!("dead-corr-2")
        );

        let second_page = tool
            .execute(json!({
                "correlation_id": "corr-a",
                "dead_letter_limit": 1,
                "dead_letter_offset": 1
            }))
            .await
            .expect("tool execution should succeed");
        assert!(second_page.success);
        let second_parsed: serde_json::Value =
            serde_json::from_str(&second_page.output).expect("output must be valid JSON");
        assert_eq!(second_parsed["dead_letters_total"], json!(2));
        assert_eq!(second_parsed["dead_letter_offset"], json!(1));
        assert_eq!(second_parsed["dead_letters_returned"], json!(1));
        assert_eq!(second_parsed["dead_letters_truncated"], json!(false));
        assert_eq!(
            second_parsed["dead_letter_next_offset"],
            serde_json::Value::Null
        );
        assert_eq!(
            second_parsed["dead_letters"][0]["message_id"],
            json!("dead-corr-0")
        );
    }

    #[tokio::test]
    async fn status_tool_applies_message_paging_with_correlation_filter() {
        let bus = test_bus();
        for (message_id, correlation_id) in [
            ("msg-corr-0", "corr-a"),
            ("msg-corr-1", "corr-b"),
            ("msg-corr-2", "corr-a"),
            ("msg-corr-3", "corr-a"),
        ] {
            let mut request = CoordinationEnvelope::new_direct(
                "delegate-lead",
                "researcher",
                format!("delegate:{correlation_id}"),
                "delegate.request",
                CoordinationPayload::DelegateTask {
                    task_id: message_id.to_string(),
                    summary: "Investigate".to_string(),
                    metadata: json!({"priority":"high"}),
                },
            );
            request.id = message_id.to_string();
            request.correlation_id = Some(correlation_id.to_string());
            bus.publish(request).expect("request should publish");
        }

        let tool = DelegateCoordinationStatusTool::new(bus, Arc::new(SecurityPolicy::default()));
        let first_page = tool
            .execute(json!({
                "agent": "researcher",
                "correlation_id": "corr-a",
                "include_messages": true,
                "message_limit": 1,
                "message_offset": 1,
                "include_dead_letters": false
            }))
            .await
            .expect("tool execution should succeed");
        assert!(first_page.success);
        let first_parsed: serde_json::Value =
            serde_json::from_str(&first_page.output).expect("output must be valid JSON");
        assert_eq!(first_parsed["inboxes"].as_array().map(Vec::len), Some(1));
        assert_eq!(first_parsed["inboxes"][0]["pending"], json!(4));
        assert_eq!(first_parsed["inboxes"][0]["pending_filtered"], json!(3));
        assert_eq!(first_parsed["inboxes"][0]["message_total"], json!(3));
        assert_eq!(first_parsed["inboxes"][0]["message_offset"], json!(1));
        assert_eq!(first_parsed["inboxes"][0]["messages_returned"], json!(1));
        assert_eq!(
            first_parsed["inboxes"][0]["messages_truncated"],
            json!(true)
        );
        assert_eq!(first_parsed["inboxes"][0]["message_next_offset"], json!(2));
        assert_eq!(
            first_parsed["inboxes"][0]["messages"][0]["message_id"],
            json!("msg-corr-2")
        );

        let second_page = tool
            .execute(json!({
                "agent": "researcher",
                "correlation_id": "corr-a",
                "include_messages": true,
                "message_limit": 1,
                "message_offset": 2,
                "include_dead_letters": false
            }))
            .await
            .expect("tool execution should succeed");
        assert!(second_page.success);
        let second_parsed: serde_json::Value =
            serde_json::from_str(&second_page.output).expect("output must be valid JSON");
        assert_eq!(second_parsed["inboxes"][0]["message_total"], json!(3));
        assert_eq!(second_parsed["inboxes"][0]["message_offset"], json!(2));
        assert_eq!(second_parsed["inboxes"][0]["messages_returned"], json!(1));
        assert_eq!(
            second_parsed["inboxes"][0]["messages_truncated"],
            json!(false)
        );
        assert_eq!(
            second_parsed["inboxes"][0]["message_next_offset"],
            serde_json::Value::Null
        );
        assert_eq!(
            second_parsed["inboxes"][0]["messages"][0]["message_id"],
            json!("msg-corr-3")
        );
    }
}
