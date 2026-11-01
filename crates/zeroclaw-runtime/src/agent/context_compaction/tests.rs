//! Tests for the context-compaction module.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::model_provider::{ChatMessage, ChatRequest, ChatResponse, ModelProvider};
use zeroclaw_config::scattered_types::ContextCompressionConfig;

use super::{CompactionRequest, compact_history};

/// Provider stub that records the summarizer prompt and returns a fixed
/// summary.
struct RecordingProvider {
    prompts: Mutex<Vec<String>>,
    fail: bool,
}

impl RecordingProvider {
    fn new() -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
            fail: true,
        }
    }
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        self.prompts
            .lock()
            .expect("prompts lock")
            .push(message.to_string());
        if self.fail {
            anyhow::bail!("summarizer unavailable");
        }
        Ok("SUMMARY".to_string())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        anyhow::bail!("compaction never uses the tool-capable chat path")
    }
}

impl Attributable for RecordingProvider {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
    }

    fn alias(&self) -> &str {
        "recording-provider"
    }
}

fn config(protect_first: usize, protect_last: usize) -> ContextCompressionConfig {
    ContextCompressionConfig {
        enabled: true,
        protect_first_n: protect_first,
        protect_last_n: protect_last,
        threshold_ratio: 0.5,
        ..ContextCompressionConfig::default()
    }
}

fn history_with_turns(turns: usize) -> Vec<ChatMessage> {
    let mut history = vec![ChatMessage::system("system prompt")];
    for turn in 0..turns {
        history.push(ChatMessage::user(format!("user turn {turn}")));
        history.push(ChatMessage::assistant(format!("assistant turn {turn}")));
    }
    history
}

async fn run(
    history: &mut Vec<ChatMessage>,
    crumb_present: &mut bool,
    provider: &RecordingProvider,
    config: &ContextCompressionConfig,
    forced: bool,
) -> super::CompactionOutcome {
    compact_history(CompactionRequest {
        history,
        crumb_present,
        provider: provider as &dyn ModelProvider,
        model: "test-model",
        temperature: None,
        config,
        context_window: 1_000_000,
        forced,
        timeout: Duration::from_secs(5),
    })
    .await
}

#[tokio::test]
async fn compaction_replaces_the_middle_turns_with_a_summary() {
    let provider = RecordingProvider::new();
    let mut history = history_with_turns(6);
    let mut crumb_present = false;
    let outcome = run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        true,
    )
    .await;

    assert!(outcome.compacted, "the forced compaction must run");
    assert_eq!(outcome.turns_compacted, 4);
    assert!(outcome.tokens_after < outcome.tokens_before);
    assert!(crumb_present, "the summary takes the synthetic head slot");
    // System prompt first, then the summary, then the protected head turn,
    // then the protected tail turn.
    assert_eq!(history[0].role, "system");
    assert!(history[1].content.contains("SUMMARY"));
    assert_eq!(history[2].content, "user turn 0");
    assert_eq!(history[3].content, "assistant turn 0");
    assert_eq!(history[4].content, "user turn 5");
    assert_eq!(history[5].content, "assistant turn 5");
    assert_eq!(history.len(), 6);
}

#[tokio::test]
async fn compaction_keeps_the_protected_head_and_tail_turns() {
    let provider = RecordingProvider::new();
    let mut history = history_with_turns(8);
    let mut crumb_present = false;
    run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(2, 2),
        true,
    )
    .await;

    let rendered: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
    assert!(
        rendered.iter().any(|c| c.contains("user turn 0")),
        "the first protected turn must survive"
    );
    assert!(
        rendered.iter().any(|c| c.contains("user turn 1")),
        "the second protected turn must survive"
    );
    assert!(
        rendered.iter().any(|c| c.contains("user turn 7")),
        "the last protected turn must survive"
    );
    assert!(
        !rendered.iter().any(|c| c.contains("user turn 3")),
        "the compacted turns must be gone"
    );
}

#[tokio::test]
async fn compaction_is_skipped_when_the_history_is_too_short() {
    let provider = RecordingProvider::new();
    let mut history = history_with_turns(2);
    let mut crumb_present = false;
    let outcome = run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        true,
    )
    .await;

    assert!(!outcome.compacted);
    assert_eq!(history.len(), 5, "the history must be untouched");
    assert!(provider.prompts.lock().expect("prompts lock").is_empty());
}

#[tokio::test]
async fn compaction_stays_under_the_threshold_unless_forced() {
    let provider = RecordingProvider::new();
    let mut history = history_with_turns(4);
    let mut crumb_present = false;
    let outcome = run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        false,
    )
    .await;

    assert!(!outcome.compacted, "a small history must not be compacted");
    assert!(provider.prompts.lock().expect("prompts lock").is_empty());
}

#[tokio::test]
async fn a_failed_summary_falls_back_to_the_transcript() {
    let provider = RecordingProvider::failing();
    let mut history = history_with_turns(6);
    let mut crumb_present = false;
    let outcome = run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        true,
    )
    .await;

    assert!(outcome.compacted, "the fallback still compacts");
    assert!(
        history[1].content.contains("user turn 2"),
        "the fallback keeps the raw transcript of the compacted turns"
    );
}

#[tokio::test]
async fn a_previous_summary_is_folded_into_the_next_one() {
    let provider = RecordingProvider::new();
    let mut history = history_with_turns(6);
    let mut crumb_present = false;
    run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        true,
    )
    .await;
    let first_summary = history[1].content.clone();

    let mut second_history = history.clone();
    for turn in 6..10 {
        second_history.push(ChatMessage::user(format!("user turn {turn}")));
        second_history.push(ChatMessage::assistant(format!("assistant turn {turn}")));
    }
    let outcome = run(
        &mut second_history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        true,
    )
    .await;

    assert!(outcome.compacted);
    let prompts = provider.prompts.lock().expect("prompts lock");
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].contains("SUMMARY"),
        "the previous summary must be part of the new transcript"
    );
    assert!(first_summary.contains("SUMMARY"));
}

#[tokio::test]
async fn the_summary_survives_the_whole_turn_trim() {
    let provider = RecordingProvider::new();
    let mut history = history_with_turns(6);
    let mut crumb_present = false;
    run(
        &mut history,
        &mut crumb_present,
        &provider,
        &config(1, 1),
        true,
    )
    .await;

    let trimmed =
        crate::agent::history_trim::trim_to_recent_turns_with_crumb(history, 1, crumb_present);
    assert!(
        trimmed
            .history
            .iter()
            .any(|m| m.content.contains("SUMMARY")),
        "the head slot must never be dropped by the trim"
    );
}
