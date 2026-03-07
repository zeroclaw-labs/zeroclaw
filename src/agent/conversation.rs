use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, Provider, ToolCall};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::fmt::Write;
use std::sync::Arc;

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

/// Keep this many most-recent non-system messages after compaction.
const COMPACTION_KEEP_RECENT_MESSAGES: usize = 20;

/// Safety cap for compaction source transcript passed to the summarizer.
const COMPACTION_MAX_SOURCE_CHARS: usize = 12_000;

/// Max characters retained in stored compaction summary.
const COMPACTION_MAX_SUMMARY_CHARS: usize = 2_000;

/// Manages conversation history, state persistence, and context optimization.
pub struct ConversationManager {
    history: Vec<ChatMessage>,
    memory: Arc<dyn Memory>,
    session_id: String,
    max_history: usize,
}

impl ConversationManager {
    pub fn new(memory: Arc<dyn Memory>, session_id: String, max_history: Option<usize>) -> Self {
        Self {
            history: Vec::new(),
            memory,
            session_id,
            max_history: max_history.unwrap_or(DEFAULT_MAX_HISTORY_MESSAGES),
        }
    }

    /// Access the raw message history.
    pub fn history(&self) -> &[ChatMessage] {
        &self.history
    }

    /// Access mutable message history.
    pub fn history_mut(&mut self) -> &mut Vec<ChatMessage> {
        &mut self.history
    }

    /// Check if the conversation is empty (ignoring system prompt).
    pub fn is_empty(&self) -> bool {
        let has_system = self.history.first().map_or(false, |m| m.role == "system");
        if has_system {
            self.history.len() <= 1
        } else {
            self.history.is_empty()
        }
    }

    /// Add a message to history without immediate checkpointing.
    pub fn add_message_silent(&mut self, message: ChatMessage) {
        self.history.push(message);
    }

    /// Update the content of the last message in history.
    pub fn update_last_message_content(&mut self, content: String) {
        if let Some(last) = self.history.last_mut() {
            last.content = content;
        }
    }

    /// Add a message to history and immediately checkpoint to memory.
    pub async fn add_message(&mut self, message: ChatMessage) -> Result<()> {
        self.history.push(message);
        self.checkpoint().await
    }

    /// Set the entire history (e.g. after loading from memory).
    pub fn set_history(&mut self, history: Vec<ChatMessage>) {
        self.history = history;
    }

    /// Attempt to load history from the memory backend.
    pub async fn load_from_memory(&mut self) -> Result<bool> {
        let key = format!("history_{}", self.session_id);
        if let Some(entry) = self.memory.get(&key).await? {
            let history: Vec<ChatMessage> = serde_json::from_str(&entry.content)?;
            self.history = history;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Persist the current history to the memory backend.
    pub async fn checkpoint(&self) -> Result<()> {
        let content = serde_json::to_string(&self.history)?;
        self.memory
            .store(
                &format!("history_{}", self.session_id),
                &content,
                MemoryCategory::Conversation,
                Some(&self.session_id),
            )
            .await?;
        Ok(())
    }

    /// Trim conversation history to prevent unbounded growth.
    pub fn trim(&mut self) {
        let has_system = self.history.first().map_or(false, |m| m.role == "system");
        let non_system_count = if has_system {
            self.history.len().saturating_sub(1)
        } else {
            self.history.len()
        };

        if non_system_count <= self.max_history {
            return;
        }

        let start = if has_system { 1 } else { 0 };
        let to_remove = non_system_count - self.max_history;
        self.history.drain(start..start + to_remove);
    }

    /// Automatically compact history using LLM summarization if it exceeds limits.
    pub async fn auto_compact(&mut self, provider: &dyn Provider, model: &str) -> Result<bool> {
        let has_system = self.history.first().map_or(false, |m| m.role == "system");
        let non_system_count = if has_system {
            self.history.len().saturating_sub(1)
        } else {
            self.history.len()
        };

        if non_system_count <= self.max_history {
            return Ok(false);
        }

        let start = if has_system { 1 } else { 0 };
        let keep_recent = COMPACTION_KEEP_RECENT_MESSAGES.min(non_system_count);
        let compact_count = non_system_count.saturating_sub(keep_recent);
        if compact_count == 0 {
            return Ok(false);
        }

        let compact_end = start + compact_count;
        let to_compact: Vec<ChatMessage> = self.history[start..compact_end].to_vec();
        let transcript = self.build_compaction_transcript(&to_compact);

        let summarizer_system = "You are a conversation compaction engine. Summarize older chat history into concise context for future turns. Preserve: user preferences, commitments, decisions, unresolved tasks, key facts. Omit: filler, repeated chit-chat, verbose tool logs. Output plain text bullet points only.";

        let summarizer_user = format!(
            "Summarize the following conversation history for context preservation. Keep it short (max 12 bullet points).\n\n{}",
            transcript
        );

        let summary_raw = provider
            .chat_with_system(Some(summarizer_system), &summarizer_user, model, 0.2)
            .await
            .unwrap_or_else(|_| {
                // Fallback to deterministic local truncation when summarization fails.
                truncate_with_ellipsis(&transcript, COMPACTION_MAX_SUMMARY_CHARS)
            });

        let summary_msg =
            ChatMessage::assistant(format!("[Compaction summary]\n{}", summary_raw.trim()));
        self.history
            .splice(start..compact_end, std::iter::once(summary_msg));

        // Persist the compacted state
        self.checkpoint().await?;

        Ok(true)
    }

    fn build_compaction_transcript(&self, messages: &[ChatMessage]) -> String {
        let mut transcript = String::new();
        for msg in messages {
            let role = msg.role.to_uppercase();
            let _ = writeln!(transcript, "{role}: {}", msg.content.trim());
        }

        if transcript.chars().count() > COMPACTION_MAX_SOURCE_CHARS {
            truncate_with_ellipsis(&transcript, COMPACTION_MAX_SOURCE_CHARS)
        } else {
            transcript
        }
    }

    /// Roll back the last user turn if it matches the provided content.
    /// Returns true if rolled back, false otherwise.
    pub async fn rollback_user_turn(&mut self, content: &str) -> bool {
        if let Some(last_pos) = self.history.iter().rposition(|m| m.role == "user") {
            if self.history[last_pos].content == content {
                self.history.remove(last_pos);
                let _ = self.checkpoint().await;
                return true;
            }
        }
        false
    }
}

/// Helper to build structured JSON for native assistant history.
pub fn build_native_assistant_history(
    text: &str,
    tool_calls: &[ToolCall],
    reasoning_content: Option<&str>,
) -> String {
    let calls_json: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "arguments": tc.arguments,
            })
        })
        .collect();

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    obj.to_string()
}

/// Helper to build structured JSON for native assistant history from parsed tool calls.
pub fn build_native_assistant_history_from_parsed_calls(
    text: &str,
    tool_calls: &[crate::agent::dispatcher::ParsedToolCall],
    reasoning_content: Option<&str>,
) -> Option<String> {
    let calls_json = tool_calls
        .iter()
        .map(|tc| {
            Some(serde_json::json!({
                "id": tc.tool_call_id.clone()?,
                "name": tc.name,
                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
            }))
        })
        .collect::<Option<Vec<_>>>()?;

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    Some(obj.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryEntry;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockMemory {
        stored: Mutex<Vec<(String, String)>>,
    }

    impl MockMemory {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }
        async fn store(
            &self,
            key: &str,
            content: &str,
            _cat: MemoryCategory,
            _sid: Option<&str>,
        ) -> Result<()> {
            self.stored
                .lock()
                .unwrap()
                .push((key.to_string(), content.to_string()));
            Ok(())
        }
        async fn recall(&self, _q: &str, _l: usize, _sid: Option<&str>) -> Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
        async fn get(&self, _k: &str) -> Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _c: Option<&MemoryCategory>,
            _sid: Option<&str>,
        ) -> Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }
        async fn forget(&self, _k: &str) -> Result<bool> {
            Ok(true)
        }
        async fn count(&self) -> Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    #[test]
    fn trim_history_preserves_system_prompt() {
        let mem = Arc::new(MockMemory::new());
        let mut cm = ConversationManager::new(mem, "test".into(), Some(10));

        cm.history_mut().push(ChatMessage::system("sys"));
        for i in 0..20 {
            cm.history_mut().push(ChatMessage::user(i.to_string()));
        }

        cm.trim();

        assert_eq!(cm.history().len(), 11);
        assert_eq!(cm.history()[0].role, "system");
        assert_eq!(cm.history()[10].content, "19");
    }

    #[test]
    fn is_empty_ignores_system_prompt() {
        let mem = Arc::new(MockMemory::new());
        let mut cm = ConversationManager::new(mem, "test".into(), None);

        assert!(cm.is_empty());

        cm.history_mut().push(ChatMessage::system("sys"));
        assert!(cm.is_empty());

        cm.history_mut().push(ChatMessage::user("hi"));
        assert!(!cm.is_empty());
    }

    #[tokio::test]
    async fn add_message_checkpoints() {
        let mem = Arc::new(MockMemory::new());
        let mut cm = ConversationManager::new(mem.clone(), "test-session".into(), None);

        cm.add_message(ChatMessage::user("hello")).await.unwrap();

        let stored = mem.stored.lock().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].0, "history_test-session");
        assert!(stored[0].1.contains("hello"));
    }

    #[test]
    fn build_compaction_transcript_formats_roles() {
        let mem = Arc::new(MockMemory::new());
        let cm = ConversationManager::new(mem, "test".into(), None);
        let messages = vec![
            ChatMessage::user("I like dark mode"),
            ChatMessage::assistant("Got it"),
        ];
        let transcript = cm.build_compaction_transcript(&messages);
        assert!(transcript.contains("USER: I like dark mode"));
        assert!(transcript.contains("ASSISTANT: Got it"));
    }
}
