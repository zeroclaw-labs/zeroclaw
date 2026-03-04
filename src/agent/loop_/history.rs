use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, Provider};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::fmt::Write;

/// Keep this many most-recent non-system messages after compaction.
const COMPACTION_KEEP_RECENT_MESSAGES: usize = 20;

/// Safety cap for compaction source transcript passed to the summarizer.
const COMPACTION_MAX_SOURCE_CHARS: usize = 12_000;

/// Max characters retained in stored compaction summary.
const COMPACTION_MAX_SUMMARY_CHARS: usize = 2_000;

/// Safety cap for durable facts extracted during pre-compaction flush.
const COMPACTION_MAX_FLUSH_FACTS: usize = 8;

/// Number of conversation turns between automatic fact extractions.
const EXTRACT_TURN_INTERVAL: usize = 5;

/// Minimum combined character count (user + assistant) to trigger extraction.
const EXTRACT_MIN_CHARS: usize = 200;

/// Safety cap for fact-extraction transcript sent to the LLM.
const EXTRACT_MAX_SOURCE_CHARS: usize = 12_000;

/// Maximum characters for the "already known facts" section injected into
/// the extraction prompt.  Keeps token cost bounded when recall returns
/// long entries.
const KNOWN_SECTION_MAX_CHARS: usize = 2_000;

/// Maximum length (in chars) for a normalized fact key.
const FACT_KEY_MAX_LEN: usize = 64;

/// Substrings that indicate a fact is purely a secret shell after redaction.
const SECRET_SHELL_PATTERNS: &[&str] = &[
    "api key",
    "api_key",
    "token",
    "password",
    "secret",
    "credential",
    "access key",
    "access_key",
    "private key",
    "private_key",
];

/// Trim conversation history to prevent unbounded growth.
/// Preserves the system prompt (first message if role=system) and the most recent messages.
pub(super) fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    // Nothing to trim if within limit
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len() - 1
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return;
    }

    let start = if has_system { 1 } else { 0 };
    let mut trim_end = start + (non_system_count - max_history);
    // Never keep a leading `role=tool` at the trim boundary. Tool-message runs
    // must remain attached to their preceding assistant(tool_calls) message.
    while trim_end < history.len() && history[trim_end].role == "tool" {
        trim_end += 1;
    }
    history.drain(start..trim_end);
}

pub(super) fn build_compaction_transcript(messages: &[ChatMessage]) -> String {
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

pub(super) fn apply_compaction_summary(
    history: &mut Vec<ChatMessage>,
    start: usize,
    compact_end: usize,
    summary: &str,
) {
    let summary_msg = ChatMessage::assistant(format!("[Compaction summary]\n{}", summary.trim()));
    history.splice(start..compact_end, std::iter::once(summary_msg));
}

/// Returns `(compacted, flush_ok)`:
/// - `compacted`: whether history was actually compacted
/// - `flush_ok`: whether the pre-compaction `flush_durable_facts` succeeded
///   (always `true` when `post_turn_active` or compaction didn't happen)
pub(super) async fn auto_compact_history(
    history: &mut Vec<ChatMessage>,
    provider: &dyn Provider,
    model: &str,
    max_history: usize,
    hooks: Option<&crate::hooks::HookRunner>,
    memory: Option<&dyn Memory>,
    session_id: Option<&str>,
    post_turn_active: bool,
) -> Result<(bool, bool)> {
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len().saturating_sub(1)
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return Ok((false, true));
    }

    let start = if has_system { 1 } else { 0 };
    let keep_recent = COMPACTION_KEEP_RECENT_MESSAGES.min(non_system_count);
    let compact_count = non_system_count.saturating_sub(keep_recent);
    if compact_count == 0 {
        return Ok((false, true));
    }

    let mut compact_end = start + compact_count;
    // Do not split assistant(tool_calls) -> tool runs across compaction boundary.
    while compact_end < history.len() && history[compact_end].role == "tool" {
        compact_end += 1;
    }
    let to_compact: Vec<ChatMessage> = history[start..compact_end].to_vec();
    let to_compact = if let Some(hooks) = hooks {
        match hooks.run_before_compaction(to_compact).await {
            crate::hooks::HookResult::Continue(messages) => messages,
            crate::hooks::HookResult::Cancel(reason) => {
                tracing::info!(%reason, "history compaction cancelled by hook");
                return Ok((false, true));
            }
        }
    } else {
        to_compact
    };
    let transcript = build_compaction_transcript(&to_compact);

    // ── Pre-compaction memory flush ──────────────────────────────────
    // Before discarding old messages, ask the LLM to extract durable
    // facts and store them as Core memories so they survive compaction.
    // Skip when post-turn extraction is active (it already covered these turns).
    let flush_ok = if post_turn_active {
        true
    } else if let Some(mem) = memory {
        flush_durable_facts(provider, model, &transcript, mem, session_id).await
    } else {
        true
    };

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

    let summary = truncate_with_ellipsis(&summary_raw, COMPACTION_MAX_SUMMARY_CHARS);
    let summary = if let Some(hooks) = hooks {
        match hooks.run_after_compaction(summary).await {
            crate::hooks::HookResult::Continue(next_summary) => next_summary,
            crate::hooks::HookResult::Cancel(reason) => {
                tracing::info!(%reason, "post-compaction summary cancelled by hook");
                return Ok((false, true));
            }
        }
    } else {
        summary
    };
    apply_compaction_summary(history, start, compact_end, &summary);

    Ok((true, flush_ok))
}

/// Extract durable facts from a conversation transcript and store them as
/// `Core` memories. Called before compaction discards old messages.
///
/// Best-effort: failures are logged but never block compaction.
/// Returns `true` when facts were stored **or** the LLM confirmed
/// there are none (`NONE` response). Returns `false` on LLM/store
/// failures so the caller can avoid marking extraction as successful.
async fn flush_durable_facts(
    provider: &dyn Provider,
    model: &str,
    transcript: &str,
    memory: &dyn Memory,
    session_id: Option<&str>,
) -> bool {
    const FLUSH_SYSTEM: &str = "\
You extract durable facts from a conversation that is about to be compacted. \
Output ONLY facts worth remembering long-term — user preferences, project decisions, \
technical constraints, commitments, or important discoveries.\n\
\n\
NEVER extract secrets, API keys, tokens, passwords, credentials, \
or any sensitive authentication data. If the conversation contains \
such data, skip it entirely.\n\
\n\
Output one fact per line, prefixed with a short key in brackets. \
Example:\n\
[preferred_language] User prefers Rust over Go\n\
[db_choice] Project uses PostgreSQL 16\n\
If there are no durable facts, output exactly: NONE";

    let flush_user = format!(
        "Extract durable facts from this conversation (max 8 facts):\n\n{}",
        transcript
    );

    let response = match provider
        .chat_with_system(Some(FLUSH_SYSTEM), &flush_user, model, 0.2)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Pre-compaction memory flush failed: {e}");
            return false;
        }
    };

    if response.trim().eq_ignore_ascii_case("NONE") {
        return true; // genuinely no facts
    }
    if response.trim().is_empty() {
        return false; // provider returned empty — treat as failure
    }

    let mut stored = 0usize;
    let mut parsed = 0usize;
    let mut store_failures = 0usize;
    for line in response.lines() {
        if stored >= COMPACTION_MAX_FLUSH_FACTS {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Parse "[key] content" format
        if let Some((key, content)) = parse_fact_line(line) {
            parsed += 1;
            // Scrub secrets from extracted content.
            let clean = crate::providers::scrub_secret_patterns(content);
            if should_skip_redacted_fact(&clean, content) {
                tracing::info!(
                    "Skipped compaction fact '{key}': only secret shell remains after redaction"
                );
                continue;
            }
            let norm_key = normalize_fact_key(key);
            if norm_key.is_empty() {
                continue;
            }
            let prefixed_key = format!("auto_{norm_key}");
            if let Err(e) = memory
                .store(&prefixed_key, &clean, MemoryCategory::Core, session_id)
                .await
            {
                tracing::warn!("Failed to store compaction fact '{prefixed_key}': {e}");
                store_failures += 1;
            } else {
                stored += 1;
            }
        }
    }
    if stored > 0 {
        tracing::info!("Pre-compaction flush: stored {stored} durable fact(s) to Core memory");
    }
    // Success when at least one fact was parsed and no store failures
    // occurred, OR all parsed facts were intentionally skipped.
    // Unparseable output (parsed == 0) is treated as failure.
    parsed > 0 && store_failures == 0
}

/// Parse a `[key] content` line from the fact extraction output.
fn parse_fact_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start_matches(|c: char| c == '-' || c.is_whitespace());
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let key = rest[..close].trim();
    let content = rest[close + 1..].trim();
    if key.is_empty() || content.is_empty() {
        return None;
    }
    Some((key, content))
}

/// Normalize a fact key to a consistent `snake_case` form with length cap.
///
/// - Replaces whitespace/hyphens with underscores
/// - Lowercases
/// - Strips non-alphanumeric (except `_`)
/// - Collapses repeated underscores
/// - Truncates to [`FACT_KEY_MAX_LEN`]
fn normalize_fact_key(raw: &str) -> String {
    let mut key: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    // Collapse repeated underscores.
    while key.contains("__") {
        key = key.replace("__", "_");
    }
    let key = key.trim_matches('_');
    if key.chars().count() > FACT_KEY_MAX_LEN {
        key.chars().take(FACT_KEY_MAX_LEN).collect()
    } else {
        key.to_string()
    }
}

// ── Post-turn fact extraction ───────────────────────────────────────

/// Accumulates conversation turns for periodic fact extraction.
///
/// Decoupled from `history` so tool/summary messages do not affect
/// the extraction window.
pub(crate) struct TurnBuffer {
    turns: Vec<(String, String)>,
    total_chars: usize,
    last_extract_succeeded: bool,
}

/// Outcome of a single extraction attempt.
pub(crate) struct ExtractionResult {
    /// Number of facts successfully stored to Core memory.
    pub stored: usize,
    /// `true` when the LLM confirmed there are no new facts (or all parsed
    /// facts were intentionally skipped). `false` on LLM/store failures.
    pub no_facts: bool,
}

impl TurnBuffer {
    pub fn new() -> Self {
        Self {
            turns: Vec::new(),
            total_chars: 0,
            last_extract_succeeded: true,
        }
    }

    /// Record a completed conversation turn.
    pub fn push(&mut self, user_msg: &str, assistant_resp: &str) {
        self.total_chars += user_msg.chars().count() + assistant_resp.chars().count();
        self.turns
            .push((user_msg.to_string(), assistant_resp.to_string()));
    }

    /// Whether the buffer has accumulated enough turns and content to
    /// justify an extraction call.
    pub fn should_extract(&self) -> bool {
        self.turns.len() >= EXTRACT_TURN_INTERVAL && self.total_chars >= EXTRACT_MIN_CHARS
    }

    /// Drain all buffered turns and return them for extraction.
    /// Resets character counter; `last_extract_succeeded` is cleared
    /// until the caller confirms success via [`mark_extract_success`].
    pub fn drain_for_extraction(&mut self) -> Vec<(String, String)> {
        self.total_chars = 0;
        self.last_extract_succeeded = false;
        std::mem::take(&mut self.turns)
    }

    /// Mark the most recent extraction as successful.
    pub fn mark_extract_success(&mut self) {
        self.last_extract_succeeded = true;
    }

    /// Whether there are buffered turns that have not been extracted.
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Whether compaction should fall back to its own `flush_durable_facts`.
    /// This returns `true` when un-extracted turns remain **or** the last
    /// extraction failed (so durable facts may have been lost).
    pub fn needs_compaction_fallback(&self) -> bool {
        !self.turns.is_empty() || !self.last_extract_succeeded
    }
}

/// Extract durable facts from recent conversation turns and store them
/// as `Core` memories.
///
/// Best-effort: failures are logged but never block the caller.
///
/// This is the unified extraction entry-point used by all agent entry
/// points (single-message, interactive, channel, `Agent` struct).
pub(crate) async fn extract_facts_from_turns(
    provider: &dyn Provider,
    model: &str,
    turns: &[(String, String)],
    memory: &dyn Memory,
    session_id: Option<&str>,
) -> ExtractionResult {
    let empty = ExtractionResult {
        stored: 0,
        no_facts: true,
    };

    if turns.is_empty() {
        return empty;
    }

    // Build transcript from buffered turns.
    let mut transcript = String::new();
    for (user, assistant) in turns {
        let _ = writeln!(transcript, "USER: {}", user.trim());
        let _ = writeln!(transcript, "ASSISTANT: {}", assistant.trim());
        transcript.push('\n');
    }

    let total_chars: usize = turns
        .iter()
        .map(|(u, a)| u.chars().count() + a.chars().count())
        .sum();
    if total_chars < EXTRACT_MIN_CHARS {
        return empty;
    }

    // Truncate to avoid oversized LLM prompts with very long messages.
    if transcript.chars().count() > EXTRACT_MAX_SOURCE_CHARS {
        transcript = truncate_with_ellipsis(&transcript, EXTRACT_MAX_SOURCE_CHARS);
    }

    // Recall existing memories for dedup context.
    let existing = memory
        .recall(&transcript, 10, session_id)
        .await
        .unwrap_or_default();

    let mut known_section = String::new();
    if !existing.is_empty() {
        known_section.push_str(
            "\nYou already know these facts (do NOT repeat them; \
             use the SAME key if a fact needs updating):\n",
        );
        for entry in &existing {
            let line = format!("- {}: {}\n", entry.key, entry.content);
            if known_section.chars().count() + line.chars().count() > KNOWN_SECTION_MAX_CHARS {
                known_section.push_str("- ... (truncated)\n");
                break;
            }
            known_section.push_str(&line);
        }
    }

    let system_prompt = format!(
        "You extract durable facts from a conversation. \
         Output ONLY facts worth remembering long-term \u{2014} user preferences, project decisions, \
         technical constraints, commitments, or important discoveries.\n\
         \n\
         NEVER extract secrets, API keys, tokens, passwords, credentials, \
         or any sensitive authentication data. If the conversation contains \
         such data, skip it entirely.\n\
         {known_section}\n\
         Output one fact per line, prefixed with a short key in brackets.\n\
         Example:\n\
         [preferred_language] User prefers Rust over Go\n\
         [db_choice] Project uses PostgreSQL 16\n\
         If there are no new durable facts, output exactly: NONE"
    );

    let user_prompt = format!(
        "Extract durable facts from this conversation (max {} facts):\n\n{}",
        COMPACTION_MAX_FLUSH_FACTS, transcript
    );

    let response = match provider
        .chat_with_system(Some(&system_prompt), &user_prompt, model, 0.2)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Post-turn fact extraction failed: {e}");
            return ExtractionResult {
                stored: 0,
                no_facts: false,
            };
        }
    };

    if response.trim().eq_ignore_ascii_case("NONE") {
        return empty;
    }
    if response.trim().is_empty() {
        // Provider returned empty — treat as failure so compaction
        // fallback remains active.
        return ExtractionResult {
            stored: 0,
            no_facts: false,
        };
    }

    let mut stored = 0usize;
    let mut parsed = 0usize;
    let mut store_failures = 0usize;
    for line in response.lines() {
        if stored >= COMPACTION_MAX_FLUSH_FACTS {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, content)) = parse_fact_line(line) {
            parsed += 1;
            // Scrub secrets from extracted content.
            let clean = crate::providers::scrub_secret_patterns(content);
            if should_skip_redacted_fact(&clean, content) {
                tracing::info!("Skipped fact '{key}': only secret shell remains after redaction");
                continue;
            }
            let norm_key = normalize_fact_key(key);
            if norm_key.is_empty() {
                continue;
            }
            let prefixed_key = format!("auto_{norm_key}");
            if let Err(e) = memory
                .store(&prefixed_key, &clean, MemoryCategory::Core, session_id)
                .await
            {
                tracing::warn!("Failed to store extracted fact '{prefixed_key}': {e}");
                store_failures += 1;
            } else {
                stored += 1;
            }
        }
    }
    if stored > 0 {
        tracing::info!("Post-turn extraction: stored {stored} durable fact(s) to Core memory");
    }

    // no_facts is true only when the LLM returned parseable facts that were
    // all intentionally skipped (e.g. redacted) — NOT when store() failed.
    // When parsed == 0 (unparseable output) or store_failures > 0 (backend
    // errors), treat as failure so compaction fallback remains active.
    ExtractionResult {
        stored,
        no_facts: parsed > 0 && stored == 0 && store_failures == 0,
    }
}

/// Decide whether a redacted fact should be skipped.
///
/// A fact is skipped when scrubbing removed secrets and the remaining
/// text is empty or consists solely of generic secret-type labels
/// (e.g. "api key", "token").
fn should_skip_redacted_fact(clean: &str, original: &str) -> bool {
    // No redaction happened — always keep.
    if clean == original {
        return false;
    }
    let remainder = clean.replace("[REDACTED]", "").trim().to_lowercase();
    let remainder = remainder.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    if remainder.is_empty() {
        return true;
    }
    SECRET_SHELL_PATTERNS.contains(&remainder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, ChatResponse, Provider};
    use async_trait::async_trait;

    struct StaticSummaryProvider;

    #[async_trait]
    impl Provider for StaticSummaryProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("- summarized context".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("- summarized context".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            })
        }
    }

    fn assistant_with_tool_call(id: &str) -> ChatMessage {
        ChatMessage::assistant(format!(
            "{{\"content\":\"\",\"tool_calls\":[{{\"id\":\"{id}\",\"name\":\"shell\",\"arguments\":\"{{}}\"}}]}}"
        ))
    }

    fn tool_result(id: &str) -> ChatMessage {
        ChatMessage::tool(format!("{{\"tool_call_id\":\"{id}\",\"content\":\"ok\"}}"))
    }

    #[test]
    fn trim_history_avoids_orphan_tool_at_boundary() {
        let mut history = vec![
            ChatMessage::user("old"),
            assistant_with_tool_call("call_1"),
            tool_result("call_1"),
            ChatMessage::user("recent"),
        ];

        trim_history(&mut history, 2);

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "recent");
    }

    #[tokio::test]
    async fn auto_compact_history_does_not_split_tool_run_boundary() {
        let mut history = vec![
            ChatMessage::user("oldest"),
            assistant_with_tool_call("call_2"),
            tool_result("call_2"),
        ];
        for idx in 0..19 {
            history.push(ChatMessage::user(format!("recent-{idx}")));
        }
        // 22 non-system messages => compaction with max_history=21 would
        // previously cut right before the tool result (index 2).
        assert_eq!(history.len(), 22);

        let compacted = auto_compact_history(
            &mut history,
            &StaticSummaryProvider,
            "test-model",
            21,
            None,
            None,
            None,
            false,
        )
        .await
        .expect("compaction should succeed");

        assert!(compacted.0);
        assert_eq!(history[0].role, "assistant");
        assert!(
            history[0].content.contains("[Compaction summary]"),
            "summary message should replace compacted range"
        );
        assert_ne!(
            history[1].role, "tool",
            "first retained message must not be an orphan tool result"
        );
    }

    #[test]
    fn parse_fact_line_extracts_key_and_content() {
        assert_eq!(
            parse_fact_line("[preferred_language] User prefers Rust over Go"),
            Some(("preferred_language", "User prefers Rust over Go"))
        );
    }

    #[test]
    fn parse_fact_line_handles_leading_dash() {
        assert_eq!(
            parse_fact_line("- [db_choice] Project uses PostgreSQL 16"),
            Some(("db_choice", "Project uses PostgreSQL 16"))
        );
    }

    #[test]
    fn parse_fact_line_rejects_empty_key_or_content() {
        assert_eq!(parse_fact_line("[] some content"), None);
        assert_eq!(parse_fact_line("[key]"), None);
        assert_eq!(parse_fact_line("[key]  "), None);
    }

    #[test]
    fn parse_fact_line_rejects_malformed_input() {
        assert_eq!(parse_fact_line("no brackets here"), None);
        assert_eq!(parse_fact_line(""), None);
        assert_eq!(parse_fact_line("[unclosed bracket"), None);
    }

    #[test]
    fn normalize_fact_key_basic() {
        assert_eq!(
            normalize_fact_key("preferred_language"),
            "preferred_language"
        );
        assert_eq!(normalize_fact_key("DB Choice"), "db_choice");
        assert_eq!(normalize_fact_key("my-cool-key"), "my_cool_key");
        assert_eq!(normalize_fact_key("  spaces  "), "spaces");
        assert_eq!(normalize_fact_key("UPPER_CASE"), "upper_case");
    }

    #[test]
    fn normalize_fact_key_collapses_underscores() {
        assert_eq!(normalize_fact_key("a___b"), "a_b");
        assert_eq!(normalize_fact_key("--key--"), "key");
    }

    #[test]
    fn normalize_fact_key_truncates_long_keys() {
        let long = "a".repeat(100);
        let result = normalize_fact_key(&long);
        assert_eq!(result.len(), FACT_KEY_MAX_LEN);
    }

    #[test]
    fn normalize_fact_key_empty_on_garbage() {
        assert_eq!(normalize_fact_key("!!!"), "");
        assert_eq!(normalize_fact_key(""), "");
    }

    #[tokio::test]
    async fn auto_compact_with_memory_stores_durable_facts() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        struct FactCapture {
            stored: Mutex<Vec<(String, String)>>,
        }

        #[async_trait]
        impl Memory for FactCapture {
            async fn store(
                &self,
                key: &str,
                content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                self.stored
                    .lock()
                    .unwrap()
                    .push((key.to_string(), content.to_string()));
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "fact-capture"
            }
        }

        /// Provider that returns facts for the first call (flush) and summary for the second (compaction).
        struct FlushThenSummaryProvider {
            call_count: Mutex<usize>,
        }

        #[async_trait]
        impl Provider for FlushThenSummaryProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<String> {
                let mut count = self.call_count.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    // flush_durable_facts call
                    Ok("[lang] User prefers Rust\n[db] PostgreSQL 16".to_string())
                } else {
                    // summarizer call
                    Ok("- summarized context".to_string())
                }
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some("- summarized context".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let mem = Arc::new(FactCapture {
            stored: Mutex::new(Vec::new()),
        });
        let provider = FlushThenSummaryProvider {
            call_count: Mutex::new(0),
        };

        let mut history: Vec<ChatMessage> = Vec::new();
        for i in 0..25 {
            history.push(ChatMessage::user(format!("msg-{i}")));
        }

        let compacted = auto_compact_history(
            &mut history,
            &provider,
            "test-model",
            21,
            None,
            Some(mem.as_ref()),
            None,
            false,
        )
        .await
        .expect("compaction should succeed");

        assert!(compacted.0);

        let stored = mem.stored.lock().unwrap();
        assert_eq!(stored.len(), 2, "should store 2 durable facts");
        assert_eq!(stored[0].0, "auto_lang");
        assert_eq!(stored[0].1, "User prefers Rust");
        assert_eq!(stored[1].0, "auto_db");
        assert_eq!(stored[1].1, "PostgreSQL 16");
    }

    #[tokio::test]
    async fn auto_compact_with_memory_caps_fact_flush_at_eight_entries() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        struct FactCapture {
            stored: Mutex<Vec<(String, String)>>,
        }

        #[async_trait]
        impl Memory for FactCapture {
            async fn store(
                &self,
                key: &str,
                content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                self.stored
                    .lock()
                    .expect("fact capture lock")
                    .push((key.to_string(), content.to_string()));
                Ok(())
            }

            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }

            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }

            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }

            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }

            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }

            async fn health_check(&self) -> bool {
                true
            }

            fn name(&self) -> &str {
                "fact-capture-cap"
            }
        }

        struct FlushManyFactsProvider {
            call_count: Mutex<usize>,
        }

        #[async_trait]
        impl Provider for FlushManyFactsProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<String> {
                let mut count = self.call_count.lock().expect("provider lock");
                *count += 1;
                if *count == 1 {
                    let lines = (0..12)
                        .map(|idx| format!("[k{idx}] fact-{idx}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(lines)
                } else {
                    Ok("- summarized context".to_string())
                }
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some("- summarized context".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let mem = Arc::new(FactCapture {
            stored: Mutex::new(Vec::new()),
        });
        let provider = FlushManyFactsProvider {
            call_count: Mutex::new(0),
        };
        let mut history = (0..30)
            .map(|idx| ChatMessage::user(format!("msg-{idx}")))
            .collect::<Vec<_>>();

        let compacted = auto_compact_history(
            &mut history,
            &provider,
            "test-model",
            21,
            None,
            Some(mem.as_ref()),
            None,
            false,
        )
        .await
        .expect("compaction should succeed");
        assert!(compacted.0);

        let stored = mem.stored.lock().expect("fact capture lock");
        assert_eq!(stored.len(), COMPACTION_MAX_FLUSH_FACTS);
        assert_eq!(stored[0].0, "auto_k0");
        assert_eq!(stored[7].0, "auto_k7");
    }

    // ── TurnBuffer unit tests ──────────────────────────────────────

    #[test]
    fn turn_buffer_should_extract_requires_interval_and_chars() {
        let mut buf = TurnBuffer::new();
        assert!(!buf.should_extract());

        // Push turns with short content — interval met but chars not.
        for i in 0..EXTRACT_TURN_INTERVAL {
            buf.push(&format!("q{i}"), "a");
        }
        assert!(!buf.should_extract());

        // Reset and push with enough chars.
        let mut buf2 = TurnBuffer::new();
        let long_msg = "x".repeat(EXTRACT_MIN_CHARS);
        for _ in 0..EXTRACT_TURN_INTERVAL {
            buf2.push(&long_msg, "reply");
        }
        assert!(buf2.should_extract());
    }

    #[test]
    fn turn_buffer_drain_clears_and_marks_pending() {
        let mut buf = TurnBuffer::new();
        buf.push("hello", "world");
        assert!(!buf.is_empty());

        let turns = buf.drain_for_extraction();
        assert_eq!(turns.len(), 1);
        assert!(buf.is_empty());
        assert!(buf.needs_compaction_fallback()); // last_extract_succeeded = false after drain
    }

    #[test]
    fn turn_buffer_mark_success_clears_fallback() {
        let mut buf = TurnBuffer::new();
        buf.push("q", "a");
        let _ = buf.drain_for_extraction();
        assert!(buf.needs_compaction_fallback());

        buf.mark_extract_success();
        assert!(!buf.needs_compaction_fallback());
    }

    #[test]
    fn turn_buffer_needs_fallback_when_not_empty() {
        let mut buf = TurnBuffer::new();
        assert!(!buf.needs_compaction_fallback());

        buf.push("q", "a");
        assert!(buf.needs_compaction_fallback());
    }

    #[test]
    fn turn_buffer_counts_chars_not_bytes() {
        let mut buf = TurnBuffer::new();
        // Each CJK char is 1 char but 3 bytes.
        let cjk = "你".repeat(EXTRACT_MIN_CHARS);
        for _ in 0..EXTRACT_TURN_INTERVAL {
            buf.push(&cjk, "ok");
        }
        assert!(buf.should_extract());
    }

    // ── should_skip_redacted_fact unit tests ───────────────────────

    #[test]
    fn skip_redacted_no_redaction_keeps_fact() {
        assert!(!should_skip_redacted_fact(
            "User prefers Rust",
            "User prefers Rust"
        ));
    }

    #[test]
    fn skip_redacted_empty_remainder_skips() {
        assert!(should_skip_redacted_fact("[REDACTED]", "sk-12345secret"));
    }

    #[test]
    fn skip_redacted_secret_shell_skips() {
        assert!(should_skip_redacted_fact(
            "api key [REDACTED]",
            "api key sk-12345secret"
        ));
        assert!(should_skip_redacted_fact(
            "token: [REDACTED]",
            "token: abc123xyz"
        ));
    }

    #[test]
    fn skip_redacted_meaningful_remainder_keeps() {
        assert!(!should_skip_redacted_fact(
            "User's deployment uses [REDACTED] for auth with PostgreSQL 16",
            "User's deployment uses sk-secret for auth with PostgreSQL 16"
        ));
    }

    // ── extract_facts_from_turns integration tests ─────────────────

    #[tokio::test]
    async fn extract_facts_stores_with_auto_prefix_and_core_category() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        #[allow(clippy::type_complexity)]
        struct CaptureMem {
            stored: Mutex<Vec<(String, String, MemoryCategory, Option<String>)>>,
        }

        #[async_trait]
        impl Memory for CaptureMem {
            async fn store(
                &self,
                key: &str,
                content: &str,
                category: MemoryCategory,
                session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                self.stored.lock().unwrap().push((
                    key.to_string(),
                    content.to_string(),
                    category,
                    session_id.map(String::from),
                ));
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "capture"
            }
        }

        struct FactExtractProvider;

        #[async_trait]
        impl Provider for FactExtractProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<String> {
                Ok("[lang] User prefers Rust\n[db] PostgreSQL 16".to_string())
            }
            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let mem = Arc::new(CaptureMem {
            stored: Mutex::new(Vec::new()),
        });
        // Build turns with enough chars to exceed EXTRACT_MIN_CHARS.
        let long_msg = "x".repeat(EXTRACT_MIN_CHARS);
        let turns = vec![(long_msg, "assistant reply".to_string())];

        let result = extract_facts_from_turns(
            &FactExtractProvider,
            "test-model",
            &turns,
            mem.as_ref(),
            Some("session-42"),
        )
        .await;

        assert_eq!(result.stored, 2);
        assert!(!result.no_facts);

        let stored = mem.stored.lock().unwrap();
        assert_eq!(stored[0].0, "auto_lang");
        assert_eq!(stored[0].1, "User prefers Rust");
        assert!(matches!(stored[0].2, MemoryCategory::Core));
        assert_eq!(stored[0].3, Some("session-42".to_string()));
        assert_eq!(stored[1].0, "auto_db");
    }

    #[tokio::test]
    async fn extract_facts_returns_no_facts_on_none_response() {
        use crate::memory::{MemoryCategory, MemoryEntry};

        struct NoopMem;

        #[async_trait]
        impl Memory for NoopMem {
            async fn store(
                &self,
                _k: &str,
                _c: &str,
                _cat: MemoryCategory,
                _s: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "noop"
            }
        }

        struct NoneProvider;

        #[async_trait]
        impl Provider for NoneProvider {
            async fn chat_with_system(
                &self,
                _sp: Option<&str>,
                _m: &str,
                _model: &str,
                _t: f64,
            ) -> anyhow::Result<String> {
                Ok("NONE".to_string())
            }
            async fn chat(
                &self,
                _r: ChatRequest<'_>,
                _m: &str,
                _t: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let long_msg = "x".repeat(EXTRACT_MIN_CHARS);
        let turns = vec![(long_msg, "resp".to_string())];
        let result = extract_facts_from_turns(&NoneProvider, "model", &turns, &NoopMem, None).await;

        assert_eq!(result.stored, 0);
        assert!(result.no_facts);
    }

    #[tokio::test]
    async fn extract_facts_below_min_chars_returns_empty() {
        use crate::memory::{MemoryCategory, MemoryEntry};

        struct NoopMem;

        #[async_trait]
        impl Memory for NoopMem {
            async fn store(
                &self,
                _k: &str,
                _c: &str,
                _cat: MemoryCategory,
                _s: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "noop"
            }
        }

        let turns = vec![("hi".to_string(), "hey".to_string())];
        let result =
            extract_facts_from_turns(&StaticSummaryProvider, "model", &turns, &NoopMem, None).await;

        assert_eq!(result.stored, 0);
        assert!(result.no_facts);
    }

    #[tokio::test]
    async fn extract_facts_unparseable_response_marks_no_facts_false() {
        use crate::memory::{MemoryCategory, MemoryEntry};

        struct NoopMem;

        #[async_trait]
        impl Memory for NoopMem {
            async fn store(
                &self,
                _k: &str,
                _c: &str,
                _cat: MemoryCategory,
                _s: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "noop"
            }
        }

        /// Provider that returns unparseable garbage (no `[key] value` format).
        struct GarbageProvider;

        #[async_trait]
        impl Provider for GarbageProvider {
            async fn chat_with_system(
                &self,
                _sp: Option<&str>,
                _m: &str,
                _model: &str,
                _t: f64,
            ) -> anyhow::Result<String> {
                Ok("This is just random text without any facts.".to_string())
            }
            async fn chat(
                &self,
                _r: ChatRequest<'_>,
                _m: &str,
                _t: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let long_msg = "x".repeat(EXTRACT_MIN_CHARS);
        let turns = vec![(long_msg, "resp".to_string())];
        let result =
            extract_facts_from_turns(&GarbageProvider, "model", &turns, &NoopMem, None).await;

        assert_eq!(result.stored, 0);
        // Unparseable output should NOT be treated as "no facts" — compaction
        // fallback should remain active.
        assert!(
            !result.no_facts,
            "unparseable LLM response must not mark extraction as successful"
        );
    }

    #[tokio::test]
    async fn extract_facts_store_failure_marks_no_facts_false() {
        use crate::memory::{MemoryCategory, MemoryEntry};

        /// Memory backend that always fails on store.
        struct FailMem;

        #[async_trait]
        impl Memory for FailMem {
            async fn store(
                &self,
                _k: &str,
                _c: &str,
                _cat: MemoryCategory,
                _s: Option<&str>,
            ) -> anyhow::Result<()> {
                anyhow::bail!("disk full")
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                false
            }
            fn name(&self) -> &str {
                "fail"
            }
        }

        /// Provider that returns valid parseable facts.
        struct FactProvider;

        #[async_trait]
        impl Provider for FactProvider {
            async fn chat_with_system(
                &self,
                _sp: Option<&str>,
                _m: &str,
                _model: &str,
                _t: f64,
            ) -> anyhow::Result<String> {
                Ok("[lang] Rust\n[db] PostgreSQL".to_string())
            }
            async fn chat(
                &self,
                _r: ChatRequest<'_>,
                _m: &str,
                _t: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some(String::new()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let long_msg = "x".repeat(EXTRACT_MIN_CHARS);
        let turns = vec![(long_msg, "resp".to_string())];
        let result = extract_facts_from_turns(&FactProvider, "model", &turns, &FailMem, None).await;

        assert_eq!(result.stored, 0);
        assert!(
            !result.no_facts,
            "store failures must not mark extraction as successful"
        );
    }

    #[tokio::test]
    async fn compaction_skips_flush_when_post_turn_active() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        struct FactCapture {
            stored: Mutex<Vec<(String, String)>>,
        }

        #[async_trait]
        impl Memory for FactCapture {
            async fn store(
                &self,
                key: &str,
                content: &str,
                _cat: MemoryCategory,
                _s: Option<&str>,
            ) -> anyhow::Result<()> {
                self.stored
                    .lock()
                    .unwrap()
                    .push((key.to_string(), content.to_string()));
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "fact-capture"
            }
        }

        let mem = Arc::new(FactCapture {
            stored: Mutex::new(Vec::new()),
        });
        struct FlushThenSummaryProvider {
            call_count: Mutex<usize>,
        }

        #[async_trait]
        impl Provider for FlushThenSummaryProvider {
            async fn chat_with_system(
                &self,
                _sp: Option<&str>,
                _m: &str,
                _model: &str,
                _t: f64,
            ) -> anyhow::Result<String> {
                let mut count = self.call_count.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    Ok("[lang] User prefers Rust\n[db] PostgreSQL 16".to_string())
                } else {
                    Ok("- summarized context".to_string())
                }
            }
            async fn chat(
                &self,
                _r: ChatRequest<'_>,
                _m: &str,
                _t: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some("- summarized context".to_string()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        // Provider that would return facts if flush_durable_facts were called.
        let provider = FlushThenSummaryProvider {
            call_count: Mutex::new(0),
        };
        let mut history = (0..25)
            .map(|i| ChatMessage::user(format!("msg-{i}")))
            .collect::<Vec<_>>();

        // With post_turn_active=true, flush_durable_facts should be skipped.
        let compacted = auto_compact_history(
            &mut history,
            &provider,
            "test-model",
            21,
            None,
            Some(mem.as_ref()),
            None,
            true, // post_turn_active
        )
        .await
        .expect("compaction should succeed");

        assert!(compacted.0);
        let stored = mem.stored.lock().unwrap();
        // No auto-extracted entries should be stored.
        assert!(
            stored.iter().all(|(k, _)| !k.starts_with("auto_")),
            "flush_durable_facts should be skipped when post_turn_active=true"
        );
    }
}
