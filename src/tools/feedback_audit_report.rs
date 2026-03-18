use super::traits::{Tool, ToolResult};
use crate::memory::Memory;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

const DEFAULT_QUERY: &str = "telegram feedback kind=";
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

pub struct FeedbackAuditReportTool {
    memory: Arc<dyn Memory>,
}

impl FeedbackAuditReportTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[derive(Debug, Clone)]
struct FeedbackAuditRecord {
    key: String,
    timestamp: String,
    score: Option<f64>,
    kind: String,
    source: String,
    sender: String,
    target_message_id: String,
    reply_target: String,
    quality: String,
    raw: String,
}

fn parse_feedback_record(entry: crate::memory::MemoryEntry) -> Option<FeedbackAuditRecord> {
    if !entry.content.starts_with("telegram feedback ") {
        return None;
    }

    let mut kind = String::from("unknown");
    let mut source = String::from("unknown");
    let mut sender = String::from("unknown");
    let mut target_message_id = String::from("unknown");
    let mut reply_target = String::from("unknown");
    let mut quality = String::from("unknown");

    for token in entry.content.split_whitespace().skip(2) {
        let Some((field, value)) = token.split_once('=') else {
            continue;
        };
        match field {
            "kind" => kind = value.to_string(),
            "source" => source = value.to_string(),
            "sender" => sender = value.to_string(),
            "target_message_id" => target_message_id = value.to_string(),
            "reply_target" => reply_target = value.to_string(),
            "quality" => quality = value.to_string(),
            _ => {}
        }
    }

    Some(FeedbackAuditRecord {
        key: entry.key,
        timestamp: entry.timestamp,
        score: entry.score,
        kind,
        source,
        sender,
        target_message_id,
        reply_target,
        quality,
        raw: entry.content,
    })
}

fn matches_filter(actual: &str, expected: Option<&str>) -> bool {
    expected.is_none_or(|value| actual.eq_ignore_ascii_case(value))
}

#[async_trait]
impl Tool for FeedbackAuditReportTool {
    fn name(&self) -> &str {
        "feedback_audit_report"
    }

    fn description(&self) -> &str {
        "Inspect recent Telegram thumbs-up/thumbs-down audit memories recorded from reply and reaction feedback events."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of audit entries to return (default: 10, max: 50)."
                },
                "kind": {
                    "type": "string",
                    "description": "Optional exact filter: positive or negative."
                },
                "source": {
                    "type": "string",
                    "description": "Optional exact filter: reply or reaction."
                },
                "sender": {
                    "type": "string",
                    "description": "Optional exact sender filter."
                },
                "query": {
                    "type": "string",
                    "description": "Optional recall query override. Defaults to 'telegram feedback kind='."
                },
                "include_raw": {
                    "type": "boolean",
                    "description": "Include the raw stored audit memory line for each hit. Defaults to false."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(DEFAULT_LIMIT, |value| value as usize)
            .clamp(1, MAX_LIMIT);
        let kind_filter = args.get("kind").and_then(serde_json::Value::as_str);
        let source_filter = args.get("source").and_then(serde_json::Value::as_str);
        let sender_filter = args.get("sender").and_then(serde_json::Value::as_str);
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(DEFAULT_QUERY);
        let include_raw = args
            .get("include_raw")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let entries = if self.memory.name() == "lifebook" {
            self.memory
                .recall(query, limit.saturating_mul(4).clamp(limit, MAX_LIMIT), None)
                .await?
        } else {
            self.memory.list(None, None).await?
        };

        let mut records: Vec<FeedbackAuditRecord> = entries
            .into_iter()
            .filter_map(parse_feedback_record)
            .filter(|record| matches_filter(&record.kind, kind_filter))
            .filter(|record| matches_filter(&record.source, source_filter))
            .filter(|record| matches_filter(&record.sender, sender_filter))
            .collect();

        if self.memory.name() != "lifebook" {
            records.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
        }

        records.truncate(limit);

        if records.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No Telegram feedback audit entries found for the requested filters.".into(),
                error: None,
            });
        }

        let mut output = format!(
            "Found {} Telegram feedback audit entr{}:\n",
            records.len(),
            if records.len() == 1 { "y" } else { "ies" }
        );
        for record in records {
            let score = record
                .score
                .map_or(String::new(), |value| format!(" score={value:.2}"));
            output.push_str(&format!(
                "- [{} via {}] sender={} target={} reply_target={} quality={} key={} timestamp={}{}\n",
                record.kind,
                record.source,
                record.sender,
                record.target_message_id,
                record.reply_target,
                record.quality,
                record.key,
                record.timestamp,
                score,
            ));
            if include_raw {
                output.push_str(&format!("  raw: {}\n", record.raw));
            }
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, SqliteMemory};
    use tempfile::TempDir;

    fn seeded_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[tokio::test]
    async fn reports_recent_feedback_audit_entries() {
        let (_tmp, mem) = seeded_mem();
        mem.store(
            "telegram_feedback:1",
            "telegram feedback kind=positive source=reply target_message_id=43 sender=alice reply_target=chat-1 quality=0.95",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "telegram_feedback:2",
            "telegram feedback kind=negative source=reaction target_message_id=44 sender=bob reply_target=chat-1 quality=0.15",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let tool = FeedbackAuditReportTool::new(mem);
        let result = tool.execute(json!({"limit": 5})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("positive"));
        assert!(result.output.contains("negative"));
        assert!(result.output.contains("sender=alice"));
    }

    #[tokio::test]
    async fn filters_by_source() {
        let (_tmp, mem) = seeded_mem();
        mem.store(
            "telegram_feedback:1",
            "telegram feedback kind=positive source=reply target_message_id=43 sender=alice reply_target=chat-1 quality=0.95",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "telegram_feedback:2",
            "telegram feedback kind=negative source=reaction target_message_id=44 sender=bob reply_target=chat-1 quality=0.15",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let tool = FeedbackAuditReportTool::new(mem);
        let result = tool
            .execute(json!({"source": "reaction"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("via reaction"));
        assert!(!result.output.contains("via reply"));
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = seeded_mem();
        let tool = FeedbackAuditReportTool::new(mem);
        assert_eq!(tool.name(), "feedback_audit_report");
        assert_eq!(tool.parameters_schema()["type"], "object");
    }
}