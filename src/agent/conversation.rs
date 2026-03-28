use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, Provider, ToolCall};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use chrono::Local;
use std::fmt::Write;
use std::sync::Arc;

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
pub const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

/// Default trigger for auto-compaction when estimated tokens exceed this threshold.
pub const DEFAULT_MAX_HISTORY_TOKENS: usize = 60_000;

/// Max size in chars for a single tool result before it gets truncated.
const TOOL_RESULT_TRUNCATION_LIMIT: usize = 5_000;

/// Hard limit for total history tokens before forced local trimming (no LLM).
/// This is a safety valve to prevent OOM when the model fails to compact.
const CRITICAL_TOKEN_LIMIT: usize = 100_000;

/// Keep this many most-recent non-system messages after compaction.
const COMPACTION_KEEP_RECENT_MESSAGES: usize = 15;

/// Safety cap for compaction source transcript passed to the summarizer.
const COMPACTION_MAX_SOURCE_CHARS: usize = 8_000;

/// Max characters retained in stored compaction summary.
const COMPACTION_MAX_SUMMARY_CHARS: usize = 1_500;

/// Manages conversation history, state persistence, and context optimization.
pub struct ConversationManager {
    history: Vec<ChatMessage>,
    memory: Arc<dyn Memory>,
    session_id: String,
    /// Native session ID from the provider (e.g. Gemini/OpenCode CLI session ID)
    native_session_id: Option<String>,
    /// Number of messages already synced to the native session
    native_session_len: usize,
    max_history_messages: usize,
    max_history_tokens: usize,
}

impl ConversationManager {
    pub fn new(memory: Arc<dyn Memory>, session_id: String, max_history_messages: Option<usize>) -> Self {
        Self {
            history: Vec::new(),
            memory,
            session_id,
            native_session_id: None,
            native_session_len: 0,
            max_history_messages: max_history_messages.unwrap_or(DEFAULT_MAX_HISTORY_MESSAGES),
            max_history_tokens: DEFAULT_MAX_HISTORY_TOKENS,
        }
    }

    /// Access the native session ID from the provider.
    pub fn native_session_id(&self) -> Option<&str> {
        self.native_session_id.as_deref()
    }

    /// Access the native session length (messages already synced).
    pub fn native_session_len(&self) -> usize {
        self.native_session_len
    }

    /// Set the native session state from the provider.
    pub fn set_native_session_state(&mut self, id: Option<String>, len: usize) {
        if self.native_session_id != id || self.native_session_len != len {
            self.native_session_id = id;
            self.native_session_len = len;
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

    /// Estimate total token usage of current history.
    pub fn estimated_tokens(&self) -> usize {
        self.history.iter().map(|m| m.estimated_tokens()).sum()
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
        
        // Safety: Always truncate oversized tool results immediately when added
        self.truncate_oversized_tool_results();
        
        // Safety: If we are way over budget, force a hard trim before checkpointing
        // to prevent 300MB JSON blobs from hitting the database.
        if self.estimated_tokens() > CRITICAL_TOKEN_LIMIT {
            tracing::warn!(
                tokens = self.estimated_tokens(),
                "History exceeds critical limit, forcing hard trim"
            );
            self.trim();
        }

        self.checkpoint().await
    }

    /// Set the entire history (e.g. after loading from memory).
    pub fn set_history(&mut self, history: Vec<ChatMessage>) {
        self.history = history;
        self.truncate_oversized_tool_results();
    }

    /// Attempt to load history from the memory backend.
    pub async fn load_from_memory(&mut self) -> Result<bool> {
        let key = format!("history_{}", self.session_id);
        let mut loaded = false;

        if let Some(entry) = self.memory.get(&key).await? {
            let history: Vec<ChatMessage> = serde_json::from_str(&entry.content)?;
            self.history = history;
            loaded = true;
        }

        // Also try to load the native session ID and length
        let native_key = format!("native_session_{}", self.session_id);
        if let Some(entry) = self.memory.get(&native_key).await? {
            self.native_session_id = Some(entry.content);
        }
        let len_key = format!("native_session_len_{}", self.session_id);
        if let Some(entry) = self.memory.get(&len_key).await? {
            if let Ok(len) = entry.content.parse::<usize>() {
                self.native_session_len = len;
            }
        }

        if loaded {
            // Clean up loaded history if it was somehow bloated
            if self.truncate_oversized_tool_results() || self.estimated_tokens() > CRITICAL_TOKEN_LIMIT {
                self.trim();
                let _ = self.checkpoint().await;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Persist the current history to the memory backend.
    pub async fn checkpoint(&self) -> Result<()> {
        let content = serde_json::to_string(&self.history)?;

        // Final sanity check on serialized size
        if content.len() > 1_000_000 { // 1MB JSON limit for history
             tracing::error!(
                len = content.len(),
                "History JSON still too large after trimming! Something is wrong."
            );
            // We still save it but this is a major warning sign
        }

        self.memory
            .store(
                &format!("history_{}", self.session_id),
                &content,
                MemoryCategory::Conversation,
                Some(&self.session_id),
            )
            .await?;

        // Also persist the native session state if present
        if let Some(ref native_id) = self.native_session_id {
            self.memory
                .store(
                    &format!("native_session_{}", self.session_id),
                    native_id,
                    MemoryCategory::Conversation,
                    Some(&self.session_id),
                )
                .await?;
            self.memory
                .store(
                    &format!("native_session_len_{}", self.session_id),
                    &self.native_session_len.to_string(),
                    MemoryCategory::Conversation,
                    Some(&self.session_id),
                )
                .await?;
        }

        Ok(())
    }
    /// Scan history for massively oversized tool results and truncate them
    /// to preserve context window without losing the tool metadata.
    /// Uses Head/Tail preservation to keep both start and end context.
    pub fn truncate_oversized_tool_results(&mut self) -> bool {
        let mut truncated_any = false;
        for msg in &mut self.history {
            if msg.role == "tool" || (msg.role == "user" && msg.content.starts_with("[Tool results]")) {
                let char_count = msg.content.chars().count();
                if char_count > TOOL_RESULT_TRUNCATION_LIMIT {
                    let omitted = char_count - TOOL_RESULT_TRUNCATION_LIMIT;
                    
                    // Keep the first 40% and the last 60% of the limit
                    // (The end of logs/outputs is usually more relevant than the middle)
                    let head_len = (TOOL_RESULT_TRUNCATION_LIMIT as f64 * 0.4) as usize;
                    let tail_len = TOOL_RESULT_TRUNCATION_LIMIT - head_len;

                    let prefix: String = msg.content.chars().take(head_len).collect();
                    let suffix: String = msg.content.chars().skip(char_count - tail_len).collect();
                    
                    msg.content = format!(
                        "{prefix}\n\n... [Output truncated: {omitted} chars removed. Middle section omitted to preserve head/tail context.] ...\n\n{suffix}"
                    );
                    truncated_any = true;
                }
            }
        }
        truncated_any
    }

    /// Trim conversation history to prevent unbounded growth.
    pub fn trim(&mut self) {
        let has_system = self.history.first().map_or(false, |m| m.role == "system");
        let non_system_count = if has_system {
            self.history.len().saturating_sub(1)
        } else {
            self.history.len()
        };

        // If we're under both limits, nothing to do.
        if non_system_count <= self.max_history_messages && self.estimated_tokens() <= self.max_history_tokens {
            return;
        }

        let start = if has_system { 1 } else { 0 };
        
        // If we are over the CRITICAL limit, we ignore max_history_messages and just 
        // purge everything except the system prompt and the very last user message.
        if self.estimated_tokens() > CRITICAL_TOKEN_LIMIT {
             if self.history.len() > start + 1 {
                let last_msg = self.history.pop().unwrap();
                self.history.truncate(start);
                self.history.push(last_msg);
                return;
             }
        }

        // Normal trim based on whichever limit was hit worse
        let over_messages = non_system_count.saturating_sub(self.max_history_messages);
        
        let mut current_tokens = self.estimated_tokens();
        let mut over_tokens = 0;
        if current_tokens > self.max_history_tokens {
            for msg in self.history[start..].iter() {
                current_tokens = current_tokens.saturating_sub(msg.estimated_tokens());
                over_tokens += 1;
                if current_tokens <= self.max_history_tokens {
                    break;
                }
            }
        }

        let to_remove = over_messages.max(over_tokens);
        // Ensure we don't remove everything if we're just over token limit
        let safe_to_remove = to_remove.min(non_system_count.saturating_sub(1));
        
        if safe_to_remove > 0 {
            self.history.drain(start..start + safe_to_remove);
        }
    }

    /// Automatically compact history using LLM summarization if it exceeds limits.
    pub async fn auto_compact(&mut self, provider: &dyn Provider, model: &str) -> Result<bool> {
        let has_system = self.history.first().map_or(false, |m| m.role == "system");
        let non_system_count = if has_system {
            self.history.len().saturating_sub(1)
        } else {
            self.history.len()
        };

        // First attempt deterministic truncation of massive tool outputs.
        // If this solves the token budget overflow, we can skip the expensive LLM compaction.
        let did_truncate = self.truncate_oversized_tool_results();

        let token_budget_threshold = (self.max_history_tokens as f64 * 0.8) as usize;

        if non_system_count <= self.max_history_messages && self.estimated_tokens() <= token_budget_threshold {
            if did_truncate {
                self.checkpoint().await?;
                return Ok(true);
            }
            return Ok(false);
        }

        // If we are ALREADY over the critical limit, we cannot call the LLM to compact
        // because the request itself will likely fail or OOM. Force a local trim.
        if self.estimated_tokens() > CRITICAL_TOKEN_LIMIT {
            self.trim();
            self.checkpoint().await?;
            return Ok(true);
        }

        let start = if has_system { 1 } else { 0 };
        let keep_recent = COMPACTION_KEEP_RECENT_MESSAGES.min(non_system_count);
        let compact_count = non_system_count.saturating_sub(keep_recent);
        if compact_count <= 1 { // Need at least 2 messages to compact usefully
            if did_truncate {
                self.checkpoint().await?;
                return Ok(true);
            }
            return Ok(false);
        }

        let compact_end = start + compact_count;
        let to_compact: Vec<ChatMessage> = self.history[start..compact_end].to_vec();
        let transcript = self.build_compaction_transcript(&to_compact);

        let summarizer_system = "You are a conversation compaction engine. Your goal is to compress older chat history while preserving CRITICAL context.

1. DURABLE FACTS: Identify any permanent facts (user preferences, architectural decisions, completed tasks, environment details).
   IMPORTANT: If there are NO new facts, output 'NONE' in this section.
2. NARRATIVE SUMMARY: Create a concise bulleted summary of what happened.

Output format:
[DURABLE FACTS]
- fact 1
- fact 2
[NARRATIVE SUMMARY]
- summary 1
- summary 2";

        let summarizer_user = format!(
            "Analyze and summarize the following conversation history for long-term preservation. Be precise and identify any durable facts mentioned:\n\n{}",
            transcript
        );

        let summary_raw = provider
            .chat_with_system(Some(summarizer_system), &summarizer_user, model, 0.2)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Auto-compaction LLM call failed, falling back to local truncation");
                truncate_with_ellipsis(&transcript, COMPACTION_MAX_SUMMARY_CHARS)
            });

        tracing::info!(summary = %summary_raw, "Auto-compaction summarizer output");

        // ── Proactive Fact Archiving ─────────────────────────
        if summary_raw.contains("[DURABLE FACTS]") {
            if let Some(facts_section) = summary_raw.split("[NARRATIVE SUMMARY]").next() {
                let facts = facts_section.replace("[DURABLE FACTS]", "").trim().to_string();
                let facts_upper = facts.to_uppercase();
                if !facts.is_empty() && facts_upper != "NONE" && facts_upper != "NONE." && !facts_upper.contains("NO NEW DURABLE FACTS") {
                    tracing::info!(facts = %facts, "Extracted durable facts for archiving");
                    // 1. Archive to SQLite (RAG-searchable)
                    let archive_key = format!("fact_archive_{}", Local::now().format("%Y-%m-%d"));
                    let _ = self.memory.store(
                        &archive_key,
                        &format!("Archived from session {}:\n{}", self.session_id, facts),
                        MemoryCategory::Daily,
                        Some(&self.session_id),
                    ).await;

                    // 2. Flush to MEMORY.md (Always-injected curated context)
                    let mut memory_md_path = std::env::current_dir()
                        .map(|p| p.join("MEMORY.md"))
                        .unwrap_or_else(|_| std::path::PathBuf::from("MEMORY.md"));
                    
                    if !memory_md_path.exists() {
                        // Try default home location
                        if let Some(base) = directories::BaseDirs::new() {
                            let alt_path = base.home_dir().join(".zeroclaw").join("workspace").join("MEMORY.md");
                            if alt_path.exists() {
                                memory_md_path = alt_path;
                            }
                        }
                    }
                    
                    if memory_md_path.exists() {
                        if let Ok(mut content) = std::fs::read_to_string(&memory_md_path) {
                            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
                            let new_facts = format!("\n### Captured {} (Session {})\n{}\n", timestamp, self.session_id, facts);
                            content.push_str(&new_facts);
                            if let Err(e) = std::fs::write(&memory_md_path, content) {
                                tracing::error!(error = %e, path = %memory_md_path.display(), "Failed to write facts to MEMORY.md");
                            } else {
                                tracing::info!(path = %memory_md_path.display(), "Successfully archived facts to MEMORY.md");
                            }
                        }
                    }
                } else {
                    tracing::info!("No durable facts extracted from summary");
                }
            }
        }

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

    #[test]
    fn truncate_oversized_tool_results_works() {
        let mem = Arc::new(MockMemory::new());
        let mut cm = ConversationManager::new(mem, "test".into(), None);
        
        let large_content = "A".repeat(2000) + "MIDDLE" + &"B".repeat(3000) + "END";
        cm.history_mut().push(ChatMessage::tool(large_content));
        
        let truncated = cm.truncate_oversized_tool_results();
        assert!(truncated);
        
        let content = &cm.history()[0].content;
        assert!(content.contains("Output truncated"));
        assert!(content.starts_with(&"A".repeat(2000)));
        assert!(content.ends_with("END"));
    }

    #[test]
    fn trim_respects_token_budget() {
        let mem = Arc::new(MockMemory::new());
        let mut cm = ConversationManager::new(mem, "test".into(), Some(100)); // allow many messages
        cm.max_history_tokens = 50; // tight token budget
        
        cm.history_mut().push(ChatMessage::system("system"));
        
        // Add 3 messages of ~40 tokens each (160 chars)
        let large_msg = "X".repeat(160);
        cm.history_mut().push(ChatMessage::user(&large_msg));
        cm.history_mut().push(ChatMessage::user(&large_msg));
        cm.history_mut().push(ChatMessage::user(&large_msg));
        
        assert_eq!(cm.history().len(), 4);
        assert!(cm.estimated_tokens() > 100);
        
        cm.trim();
        
        // System + 1 large message = ~40 tokens, which fits in 50
        assert!(cm.history().len() < 4);
        assert_eq!(cm.history()[0].role, "system");
        assert!(cm.estimated_tokens() <= 50);
    }
}
