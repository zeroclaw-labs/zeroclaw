//! Session types — header, messages, content blocks, usage, and session index entries.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Role of a message participant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// A single content block inside an agent message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    Thinking {
        thinking: String,
    },
}

/// Normalized token usage across providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NormalizedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

/// Header written as the first JSONL line of every transcript file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionHeader {
    pub session_id: String,
    pub created_at: String,
    pub model: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// A single agent message persisted as one JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentMessage {
    /// Unique message ID for idempotent appends. When present, duplicate
    /// messages with the same `message_id` can be detected and skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub usage: Option<NormalizedUsage>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// An entry in the session index (sessions.json).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEntry {
    pub session_id: String,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    pub model: String,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_header_roundtrip() {
        let header = SessionHeader {
            session_id: "sess-001".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            model: "claude-3".into(),
            system_prompt: Some("You are helpful.".into()),
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&header).unwrap();
        let parsed: SessionHeader = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn agent_message_with_content_blocks() {
        let msg = AgentMessage {
            message_id: None,
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Hello".into(),
                },
                ContentBlock::ToolUse {
                    id: "tu-1".into(),
                    name: "search".into(),
                    input: serde_json::json!({"q": "rust"}),
                },
            ],
            timestamp: Some("2025-01-01T00:00:01Z".into()),
            usage: Some(NormalizedUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            model: Some("claude-3".into()),
            metadata: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn normalized_usage_default() {
        let usage = NormalizedUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
    }

    #[test]
    fn session_entry_roundtrip() {
        let entry = SessionEntry {
            session_id: "sess-001".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: None,
            model: "claude-3".into(),
            message_count: 5,
            total_input_tokens: 100,
            total_output_tokens: 200,
            transcript_path: Some("transcripts/sess-001.jsonl".into()),
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: SessionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn role_serialization() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn content_block_variants() {
        let text = ContentBlock::Text { text: "hi".into() };
        let json = serde_json::to_string(&text).unwrap();
        assert!(json.contains("\"type\":\"text\""));

        let tool_result = ContentBlock::ToolResult {
            tool_use_id: "tu-1".into(),
            content: "done".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&tool_result).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));

        let thinking = ContentBlock::Thinking {
            thinking: "hmm".into(),
        };
        let json = serde_json::to_string(&thinking).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
    }
}
