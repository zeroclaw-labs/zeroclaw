//! The summarizer call that turns a compacted region into a summary.

use std::time::Duration;

use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_config::scattered_types::ContextCompressionConfig;
use zeroclaw_providers::ProviderDispatch;

use super::CompactionRegion;
use super::region::{strip_media_markers, truncate_chars};

/// System prompt for the summarizer: no tools, no autonomy, one summary.
const SUMMARIZER_SYSTEM: &str = "\
You are a conversation compaction engine. Summarize the conversation segment below \
into concise context.

PRESERVE exactly:
- All identifiers (UUIDs, hashes, file paths, URLs, tokens, IPs)
- Actions taken (tool calls, file operations, commands run)
- Key information obtained (data, results, error messages)
- Decisions made and user preferences expressed
- Current task status and unresolved items
- Constraints and requirements mentioned

OMIT:
- Verbose tool output (keep only key results)
- Repeated greetings or filler
- Redundant information already stated

Output concise bullet points. Be thorough but brief.";

/// Summarize the compacted region.
///
/// A failed or timed-out summary falls back to a truncated transcript: the
/// compaction still shrinks the request, and the caller keeps the history
/// usable instead of losing the turn outright.
pub(crate) async fn summarize(
    region: &CompactionRegion,
    provider: &dyn ModelProvider,
    model: &str,
    temperature: Option<f64>,
    config: &ContextCompressionConfig,
    timeout: Duration,
) -> String {
    let source = truncate_chars(
        &strip_media_markers(&region.transcript),
        config.source_max_chars,
    );
    if source.trim().is_empty() {
        return String::new();
    }
    let prompt = build_prompt(region, &source, config);
    let dispatcher = ProviderDispatch::from_ref(provider);
    let call = dispatcher.chat_with_system(Some(SUMMARIZER_SYSTEM), &prompt, model, temperature);
    let raw = match tokio::time::timeout(timeout, call).await {
        Ok(Ok(summary)) => summary,
        Ok(Err(error)) => {
            log_failure(&error.to_string());
            truncate_chars(&source, config.summary_max_chars)
        }
        Err(_) => {
            log_timeout(timeout);
            truncate_chars(&source, config.summary_max_chars)
        }
    };
    truncate_chars(&raw, config.summary_max_chars)
}

/// Build the summarizer prompt for one compacted region.
fn build_prompt(
    region: &CompactionRegion,
    source: &str,
    config: &ContextCompressionConfig,
) -> String {
    let identifier_note = if config.identifier_policy == "strict" {
        "\nIMPORTANT: Preserve all identifiers exactly as they appear."
    } else {
        ""
    };
    format!(
        "Summarize the following conversation history ({turns} turns, {messages} messages) \
         for context preservation. Keep it concise (max 20 bullet points).{identifier_note}\n\n{source}",
        turns = region.turns,
        messages = region.messages,
    )
}

/// Log a failed summary call without failing the turn.
fn log_failure(error: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({"error": error})),
        "Summarization LLM call failed, falling back to a truncated transcript"
    );
}

/// Log a timed-out summary call without failing the turn.
fn log_timeout(timeout: Duration) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({"timeout_secs": timeout.as_secs()})),
        "Summarization timed out, falling back to a truncated transcript"
    );
}
