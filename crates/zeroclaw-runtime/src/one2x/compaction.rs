//! Enhanced compaction: multi-stage summarization + quality safeguard.
//!
//! Replaces single-pass `compress_once` with a chunked approach that
//! preserves more detail in long conversations, plus a quality check
//! that verifies the latest user request survives compression.
//!
//! ## Upstream integration
//!
//! The hook is called from `context_compressor.rs` via:
//! ```ignore
//! #[cfg(feature = "one2x")]
//! if let Some(result) = crate::one2x::compaction::try_multi_stage_compress(
//!     &self, history, provider, model,
//! ).await? { return Ok(result); }
//! ```

use std::fmt::Write;
use std::time::Duration;

use anyhow::Result;

use crate::agent::context_compressor::{
    CompressionResult, ContextCompressionConfig, estimate_tokens,
};
use std::sync::Arc;
use zeroclaw_memory::Memory;
use zeroclaw_providers::{ChatMessage, Provider};

/// System prompt for pre-compaction key-facts extraction.
/// Runs before chunking so facts survive even if later stages fail.
/// Pattern from openclaw's pre-compaction memory flush to dated memory files.
const KEY_FACTS_EXTRACTOR_SYSTEM: &str = "\
You are a memory extraction engine. Before conversation history is compressed and \
partially lost, extract the most important persistent facts for future sessions.

Extract ONLY:
- Identifiers that must survive (UUIDs, tokens, hashes, keys, version numbers, file paths)
- Decisions made and WHY they were made
- Configuration values explicitly set by the user
- Ongoing tasks and their current state
- User preferences and constraints discovered in this session
- Critical errors and their resolutions

Format as concise bullet points. Omit greetings, filler, and anything easily re-derivable \
from project files. Max 20 bullets.";

const STAGE_SUMMARIZER_SYSTEM: &str = "\
You are a conversation compaction engine processing one chunk of a longer conversation.

PRESERVE exactly:
- All identifiers (UUIDs, hashes, file paths, URLs, tokens, IPs, version numbers)
- Actions taken (tool calls, file operations, commands run) and their outcomes
- Key data obtained (results, error messages, status codes)
- Decisions made and rationale
- User preferences and constraints
- Unresolved items and open questions

OMIT:
- Verbose tool output beyond key results
- Greetings, filler, acknowledgements
- Information already covered in a prior chunk summary

Output concise bullet points grouped by topic. Max 15 bullets per chunk.";

const MERGE_SUMMARIZER_SYSTEM: &str = "\
You are a context merge engine. Combine multiple chunk summaries into one cohesive \
context summary. Deduplicate overlapping facts. Preserve ALL identifiers exactly. \
Organize by topic. Max 25 bullet points total.";

const QUALITY_CHECK_SYSTEM: &str = "\
You are a quality checker. Given a compression summary and the latest user message, \
answer ONLY 'PASS' or 'FAIL'.

Answer 'PASS' if the summary preserves enough context to understand and respond to \
the latest user message. Answer 'FAIL' if critical context needed to answer the user \
is missing from the summary.";

/// Chunk size in messages for multi-stage compression.
/// Each chunk is summarized independently, then merged.
const CHUNK_SIZE: usize = 20;

/// Maximum chars per chunk transcript sent to the summarizer.
const CHUNK_MAX_CHARS: usize = 15_000;

/// Multi-stage compression: split middle section into chunks, summarize each,
/// then merge all chunk summaries into one final summary.
///
/// Returns `None` if the history is too short for multi-stage (falls back to
/// the upstream single-pass).
pub async fn try_multi_stage_compress(
    config: &ContextCompressionConfig,
    context_window: usize,
    memory: &Option<Arc<dyn Memory>>,
    history: &mut Vec<ChatMessage>,
    provider: &dyn Provider,
    model: &str,
) -> Result<Option<CompressionResult>> {
    let tokens_before = estimate_tokens(history);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let threshold = (context_window as f64 * config.threshold_ratio) as usize;

    if tokens_before <= threshold {
        return Ok(None);
    }

    let n = history.len();
    let protected_total = config.protect_first_n + config.protect_last_n;
    if n <= protected_total {
        return Ok(None);
    }

    let start = config.protect_first_n.min(n);
    let end = n.saturating_sub(config.protect_last_n);
    let middle_len = end.saturating_sub(start);

    // Only use multi-stage when middle section is large enough to benefit
    if middle_len < CHUNK_SIZE * 2 {
        return Ok(None); // fall back to single-pass
    }

    let summary_model = config.summary_model.as_deref().unwrap_or(model);
    let timeout = Duration::from_secs(config.timeout_secs);

    // ── Pre-compaction key-facts flush ───────────────────────────────────────
    // Extract key facts BEFORE compaction so they survive even if compaction
    // fails or the compressed summary loses detail.  Stored with a dated key
    // so future sessions can look up by date.  Mirrors openclaw's pre-compaction
    // memory flush to `memory/YYYY-MM-DD.md`.
    if let Some(mem) = memory {
        let transcript = build_chunk_transcript(&history[start..end], 12_000);
        if !transcript.is_empty() {
            let facts_prompt = format!(
                "Extract key persistent facts from this conversation history ({} messages):\n\n{}",
                middle_len, transcript
            );
            match tokio::time::timeout(
                Duration::from_secs(20),
                provider.chat_with_system(
                    Some(KEY_FACTS_EXTRACTOR_SYSTEM),
                    &facts_prompt,
                    summary_model,
                    0.1,
                ),
            )
            .await
            {
                Ok(Ok(facts)) if !facts.trim().is_empty() => {
                    let date_key = format!("key_facts_{}", chrono::Utc::now().format("%Y-%m-%d"));
                    if let Err(e) = mem
                        .store(
                            &date_key,
                            &facts,
                            zeroclaw_memory::MemoryCategory::Daily,
                            None,
                        )
                        .await
                    {
                        tracing::debug!(error = %e, "Pre-compaction key-facts flush failed (non-fatal)");
                    } else {
                        tracing::info!(
                            key = date_key,
                            "Pre-compaction key facts flushed to memory"
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, "Key-facts extraction failed (non-fatal)");
                }
                Err(_) => {
                    tracing::debug!("Key-facts extraction timed out (non-fatal)");
                }
                _ => {}
            }
        }
    }

    // Stage 1: chunk the middle and summarize each chunk
    let middle = &history[start..end];
    let chunks: Vec<&[ChatMessage]> = middle.chunks(CHUNK_SIZE).collect();
    let chunk_count = chunks.len();

    tracing::info!(
        chunk_count,
        middle_len,
        "Multi-stage compaction: summarizing {} chunks",
        chunk_count
    );

    let mut chunk_summaries = Vec::with_capacity(chunk_count);
    for (i, chunk) in chunks.iter().enumerate() {
        let transcript = build_chunk_transcript(chunk, CHUNK_MAX_CHARS);
        if transcript.is_empty() {
            continue;
        }

        let prompt = format!(
            "Summarize chunk {}/{} of the conversation ({} messages).\n\n{}",
            i + 1,
            chunk_count,
            chunk.len(),
            transcript
        );

        match tokio::time::timeout(
            timeout,
            provider.chat_with_system(Some(STAGE_SUMMARIZER_SYSTEM), &prompt, summary_model, 0.1),
        )
        .await
        {
            Ok(Ok(s)) => chunk_summaries.push(s),
            Ok(Err(e)) => {
                tracing::warn!(chunk = i, error = %e, "Chunk summarization failed");
                chunk_summaries.push(truncate_str(&transcript, 1000));
            }
            Err(_) => {
                tracing::warn!(chunk = i, "Chunk summarization timed out");
                chunk_summaries.push(truncate_str(&transcript, 1000));
            }
        }
    }

    // Stage 2: merge chunk summaries
    let merged_input = chunk_summaries
        .iter()
        .enumerate()
        .fold(String::new(), |mut acc, (i, s)| {
            let _ = writeln!(acc, "## Chunk {} Summary\n{}\n", i + 1, s);
            acc
        });

    let merge_prompt = format!(
        "Merge these {} chunk summaries into one cohesive context summary:\n\n{}",
        chunk_count, merged_input
    );

    let final_summary = match tokio::time::timeout(
        timeout,
        provider.chat_with_system(
            Some(MERGE_SUMMARIZER_SYSTEM),
            &merge_prompt,
            summary_model,
            0.1,
        ),
    )
    .await
    {
        Ok(Ok(s)) => truncate_str(&s, config.summary_max_chars),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Merge summarization failed, concatenating chunks");
            truncate_str(&merged_input, config.summary_max_chars)
        }
        Err(_) => {
            tracing::warn!("Merge summarization timed out, concatenating chunks");
            truncate_str(&merged_input, config.summary_max_chars)
        }
    };

    // Stage 3: quality check — verify latest user message is addressable
    let latest_user_msg = history
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    if !latest_user_msg.is_empty() {
        let quality_prompt = format!(
            "Summary:\n{}\n\nLatest user message:\n{}",
            &final_summary, &latest_user_msg
        );

        match tokio::time::timeout(
            Duration::from_secs(15),
            provider.chat_with_system(
                Some(QUALITY_CHECK_SYSTEM),
                &quality_prompt,
                summary_model,
                0.0,
            ),
        )
        .await
        {
            Ok(Ok(response)) if response.trim().to_uppercase().contains("FAIL") => {
                tracing::warn!(
                    "Compaction quality check FAILED — latest user request not preserved. \
                     Keeping more recent messages."
                );
                // Quality failed: protect more tail messages and retry with single-pass
                return Ok(None);
            }
            Ok(Ok(_)) => {
                tracing::debug!("Compaction quality check passed");
            }
            _ => {
                // Quality check itself failed/timed out — proceed anyway
                tracing::debug!("Quality check skipped (error/timeout)");
            }
        }
    }

    // Persist to memory before discarding
    if let Some(memory) = memory {
        let key = format!("compressed_context_{}", uuid::Uuid::new_v4());
        if let Err(e) = memory
            .store(
                &key,
                &final_summary,
                zeroclaw_memory::MemoryCategory::Daily,
                None,
            )
            .await
        {
            tracing::debug!("Failed to persist compression summary: {e}");
        }
    }

    // Splice
    let message_count = end - start;
    let summary_msg = ChatMessage::assistant(format!(
        "[CONTEXT SUMMARY \u{2014} {message_count} messages compressed via multi-stage]\n\n{final_summary}"
    ));
    history.splice(start..end, std::iter::once(summary_msg));

    // Repair orphans
    crate::agent::context_compressor::repair_tool_pairs(history);

    let tokens_after = estimate_tokens(history);
    tracing::info!(
        tokens_before,
        tokens_after,
        chunk_count,
        "Multi-stage compaction complete"
    );

    Ok(Some(CompressionResult {
        compressed: true,
        tokens_before,
        tokens_after,
        passes_used: 1,
    }))
}

fn build_chunk_transcript(messages: &[ChatMessage], max_chars: usize) -> String {
    let mut transcript = String::new();
    for msg in messages {
        let role = msg.role.to_uppercase();
        let _ = writeln!(transcript, "{role}: {}", msg.content.trim());
    }
    if transcript.len() > max_chars {
        truncate_str(&transcript, max_chars)
    } else {
        transcript
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = s[..end].to_string();
    result.push_str("...");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn test_build_chunk_transcript() {
        let msgs = vec![msg("user", "hello"), msg("assistant", "hi")];
        let t = build_chunk_transcript(&msgs, 10_000);
        assert!(t.contains("USER: hello"));
        assert!(t.contains("ASSISTANT: hi"));
    }

    #[test]
    fn test_build_chunk_transcript_truncates() {
        let msgs = vec![msg("user", &"x".repeat(20_000))];
        let t = build_chunk_transcript(&msgs, 100);
        assert!(t.len() <= 103);
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello world", 5), "hello...");
        assert_eq!(truncate_str("short", 100), "short");
    }
}
