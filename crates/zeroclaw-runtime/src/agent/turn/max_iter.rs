//! The max-iteration exit: when the loop exhausts its iterations, ask the
//! LLM for a tools-free final summary (with step timeout + cancel select)
//! and return it appended to the accumulated display text, or bail.

use super::knobs::{LoopKnobs, MaxIterationBehavior};
use super::outcome::ToolLoopCancelled;
use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::turn_stop::{TurnStop, TurnStopCode};
use zeroclaw_config::schema::PacingConfig;
use zeroclaw_providers::{ChatMessage, ModelProvider};
use zeroclaw_tool_call_parser::{strip_think_tags, strip_trailing_terminal_markers};

/// The iteration-cap stop, shared by the `ErrorAtCap` exit and the
/// graceful-summary failure paths so all three carry one code and one message.
fn max_iterations_stop(max_iterations: usize) -> TurnStop {
    TurnStop::close_out(
        TurnStopCode::MaxIterations,
        format!("Agent exceeded maximum tool iterations ({max_iterations})"),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn finish_after_max_iterations(
    model_provider: &dyn ModelProvider,
    history: &mut Vec<ChatMessage>,
    provider_name: &str,
    model: &str,
    temperature: Option<f64>,
    pacing: &PacingConfig,
    cancellation_token: Option<&CancellationToken>,
    max_iterations: usize,
    mut accumulated_display_text: String,
    turn_id: &str,
    knobs: &LoopKnobs,
    event_tx: Option<&Sender<TurnEvent>>,
    mut new_messages_out: Option<&mut Vec<ChatMessage>>,
) -> Result<String> {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "model": model,
                "max_iterations": max_iterations,
                "trace_id": turn_id,
            })),
        "tool_loop_exhausted"
    );

    // ErrorAtCap callers (embedders driving Agent::turn) treat the cap as a
    // control signal: bail instead of spending another LLM call on a summary.
    if knobs.max_iteration_behavior == MaxIterationBehavior::ErrorAtCap {
        return Err(max_iterations_stop(max_iterations).into());
    }

    // Graceful shutdown: ask the LLM for a final summary without tools
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_category(::zeroclaw_log::EventCategory::Agent)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"max_iterations": max_iterations})),
        "Max iterations reached, requesting final summary"
    );
    let tool_calls_stripped =
        crate::agent::history_pruner::strip_orphaned_tool_calls_from_assistants(history);
    let tool_messages_removed =
        crate::agent::history_pruner::remove_orphaned_tool_messages(history).removed;
    if tool_calls_stripped > 0 || tool_messages_removed > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "tool_calls_stripped": tool_calls_stripped,
                    "tool_messages_removed": tool_messages_removed,
                })),
            "Sanitised orphaned tool_use/tool_result pairing before graceful shutdown"
        );
    }

    let summary_prompt = ChatMessage::user(
        "You have reached the maximum number of tool iterations. \
         Please provide your best answer based on the work completed so far. \
         Summarize what you accomplished and what remains to be done."
            .to_string(),
    );
    let summary_prompt_mirror = summary_prompt.clone();
    history.push(summary_prompt);

    enum SummaryCall {
        Cancelled,
        TimedOut(u64),
        Done(Result<zeroclaw_providers::ChatResponse>),
    }
    let summary_call = {
        let summary_request = zeroclaw_providers::ChatRequest {
            messages: history,
            tools: None, // No tools — force a text response
            thinking: zeroclaw_api::NATIVE_THINKING_OVERRIDE
                .try_with(Clone::clone)
                .ok()
                .flatten(),
        };
        let access = crate::agent::turn::execution::ResolvedModelAccess {
            model_provider,
            provider_name,
            model,
            temperature,
        };
        // Route the graceful-summary call through the metered provider seam. This
        // was the one tool-loop provider call that skipped the budget check and
        // recorded no cost; through the seam it now fails closed when the turn's
        // budget is exhausted and its token usage is charged like any in-loop
        // call. Metering is a no-op when the turn is unscoped.
        let summary_future = access.run_model_query(summary_request);
        match pacing.step_timeout_secs {
            Some(step_secs) if step_secs > 0 => {
                let step_timeout = Duration::from_secs(step_secs);
                if let Some(token) = cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => SummaryCall::Cancelled,
                        result = tokio::time::timeout(step_timeout, summary_future) => match result {
                            Ok(inner) => SummaryCall::Done(inner),
                            Err(_) => SummaryCall::TimedOut(step_secs),
                        },
                    }
                } else {
                    match tokio::time::timeout(step_timeout, summary_future).await {
                        Ok(inner) => SummaryCall::Done(inner),
                        Err(_) => SummaryCall::TimedOut(step_secs),
                    }
                }
            }
            _ => {
                if let Some(token) = cancellation_token {
                    tokio::select! {
                        () = token.cancelled() => SummaryCall::Cancelled,
                        result = summary_future => SummaryCall::Done(result),
                    }
                } else {
                    SummaryCall::Done(summary_future.await)
                }
            }
        }
    };

    let resp = match summary_call {
        SummaryCall::Cancelled => {
            history.pop();
            return Err(ToolLoopCancelled.into());
        }
        SummaryCall::TimedOut(step_secs) => {
            history.pop();
            return Err(TurnStop::close_out(
                TurnStopCode::MaxIterations,
                format!("Final summary LLM call timed out after {step_secs}s (step_timeout_secs)"),
            )
            .into());
        }
        SummaryCall::Done(Err(e)) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Provider)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model": model,
                        "provider": provider_name,
                        "max_iterations": max_iterations,
                        "trace_id": turn_id,
                        "error": format!("{e}"),
                    })),
                "final summary LLM call failed after iteration exhaustion; bailing"
            );
            history.pop();
            // The provider error stays the source (master keeps the cause
            // chain here); the stop rides alongside it so the exit is typed
            // without losing what actually failed.
            return Err(zeroclaw_api::turn_stop::tag(
                e.context(format!(
                    "Agent exceeded maximum tool iterations ({max_iterations})"
                )),
                max_iterations_stop(max_iterations),
            ));
        }
        SummaryCall::Done(Ok(resp)) => resp,
    };

    let raw_text = resp.text.unwrap_or_default();
    if raw_text.is_empty() {
        history.pop();
        return Err(max_iterations_stop(max_iterations).into());
    }
    // The summary is raw provider text, and emitting it as a chunk makes this
    // a new automatic display sink: ACP renders `agent_message_chunk` live,
    // while gateway and RPC forward the same chunk without another
    // normalization step. Apply the display hygiene the ordinary final
    // response already gets before anything is emitted -- strip hidden think
    // content and trailing terminal markers, then withhold text that looks
    // like an internal tool-protocol envelope. The summary call passes
    // `tools: None`, so the tools-free detector is the matching contract.
    let display_text = strip_trailing_terminal_markers(&strip_think_tags(&raw_text));
    let protocol_suppressed =
        super::protocol_detect::detect_internal_protocol_without_tools(&display_text).is_some();
    if protocol_suppressed {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_category(::zeroclaw_log::EventCategory::Tool)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model": model,
                    "max_iterations": max_iterations,
                    "trace_id": turn_id,
                    "error": "malformed internal tool protocol omitted from max-iteration summary",
                })),
            "max_iteration_summary_protocol_suppressed"
        );
    }
    let display_text = if protocol_suppressed {
        crate::i18n::get_required_cli_string("channel-runtime-malformed-tool-output").to_string()
    } else {
        display_text
    };
    if display_text.trim().is_empty() {
        history.pop();
        anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
    }
    // History and result payloads keep the unmodified provider text; only the
    // display path is normalized, matching the final-response contract.
    let summary_msg = ChatMessage::assistant(raw_text.clone());
    if let Some(out) = &mut new_messages_out {
        out.push(summary_prompt_mirror);
        out.push(summary_msg.clone());
    }
    history.push(summary_msg);
    // Graceful shutdown with a visible reason so the user knows why the
    // agent stopped making progress.
    let stop_reason = crate::i18n::get_required_cli_string_with_args(
        "turn-max-iterations-reached",
        &[("max_iterations", &max_iterations.to_string())],
    );
    let segment = format!("{display_text}\n\n{stop_reason}");
    // This summary is the turn's only visible output on the max-iteration
    // exit path, and it comes from a fresh non-streaming call — there is no
    // live delta a client could have already received, so unlike the normal
    // final-response path this emit needs no live-vs-post-hoc guard. Only the
    // newly-produced segment goes out; `accumulated_display_text` holds
    // narration earlier iterations already streamed, and resending it here
    // would duplicate it in the client.
    super::events::emit_posthoc_turn_chunk(event_tx, &segment).await;
    accumulated_display_text.push_str(&segment);
    Ok(accumulated_display_text)
}

#[cfg(test)]
mod graceful_summary_metering_tests {
    use super::finish_after_max_iterations;
    use crate::agent::cost::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
    use crate::agent::turn::LoopKnobs;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{
        ChatRequest, ChatResponse, SemanticEmptyTerminalCompletion,
    };
    use zeroclaw_config::schema::{CostConfig, PacingConfig};
    use zeroclaw_providers::traits::TokenUsage;
    use zeroclaw_providers::{ChatMessage, ModelProvider};

    use super::{Sender, TurnEvent};

    /// Provider stub that counts calls and returns a summary WITH token usage.
    struct CountingUsageProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelProvider for CountingUsageProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("wrap-up summary".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: Some("wrap-up summary".to_string()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            })
        }
    }

    impl Attributable for CountingUsageProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "counting-usage-provider"
        }
    }

    async fn run_summary_with_events(
        provider: &dyn ModelProvider,
        accumulated_display_text: String,
        event_tx: Option<&Sender<TurnEvent>>,
    ) -> anyhow::Result<String> {
        let mut history = vec![ChatMessage::user("do the work")];
        let pacing = PacingConfig::default();
        let knobs = LoopKnobs::default(); // GracefulSummary
        finish_after_max_iterations(
            provider,
            &mut history,
            "custom",
            "test-model",
            None,
            &pacing,
            None,
            2,
            accumulated_display_text,
            "trace-req-test",
            &knobs,
            event_tx,
            None,
        )
        .await
    }

    async fn run_summary(provider: &dyn ModelProvider) -> anyhow::Result<String> {
        run_summary_with_events(provider, String::new(), None).await
    }

    // The graceful summary now routes through the metered provider seam: under a
    // cost-tracking scope its token usage is recorded (before this change the
    // summary recorded nothing).
    #[tokio::test]
    async fn graceful_summary_records_usage_through_the_metered_seam() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = CountingUsageProvider {
            calls: Arc::clone(&calls),
        };
        let ctx = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = Arc::clone(&ctx.turn_usage);

        let out = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async { run_summary(&provider).await })
            .await
            .expect("graceful summary should succeed");

        assert!(out.contains("wrap-up summary"), "unexpected summary: {out}");
        // The returned display text must carry both the summary and the visible
        // stop reason — deleting the stop-reason append would leave this green
        // on `wrap-up summary` alone, so the stop-reason assertion pins the
        // user-observed contract.
        assert!(
            out.contains("Turn stopped: reached maximum tool iterations (2)"),
            "stop reason with iteration count must reach returned output: {out}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "provider called once");
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 100);
        assert_eq!(recorded.output_tokens, 20);
    }

    // The graceful summary now fails closed on budget exhaustion: it was the one
    // tool-loop provider call that skipped the budget check. A tripped budget
    // (negative limit) makes the seam bail BEFORE spending, so the provider is
    // never called and the cap is surfaced as an error.
    #[tokio::test]
    async fn graceful_summary_is_budget_gated_and_skips_the_provider_when_over_budget() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = CountingUsageProvider {
            calls: Arc::clone(&calls),
        };
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = CostConfig {
            enabled: true,
            daily_limit_usd: -1.0,
            monthly_limit_usd: -1.0,
            ..CostConfig::default()
        };
        let tracker = Arc::new(crate::cost::CostTracker::new(cfg, tmp.path()).unwrap());
        let ctx = ToolLoopCostTrackingContext::new(tracker, Arc::new(HashMap::new()));

        let result = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async { run_summary(&provider).await })
            .await;

        assert!(result.is_err(), "over-budget summary must bail, not spend");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "budget gate must fire before the provider call"
        );
    }

    struct SemanticEmptySummaryProvider;

    #[async_trait]
    impl ModelProvider for SemanticEmptySummaryProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("<think>internal reasoning</think>".to_string()),
                tool_calls: Vec::new(),
                usage: Some(TokenUsage {
                    input_tokens: Some(100),
                    output_tokens: Some(20),
                    cached_input_tokens: None,
                }),
                reasoning_content: Some("internal reasoning".to_string()),
            })
        }
    }

    impl Attributable for SemanticEmptySummaryProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "semantic-empty-summary-provider"
        }
    }

    #[tokio::test]
    async fn graceful_summary_rejects_think_only_text_with_rejected_usage_and_typed_cause() {
        let provider = SemanticEmptySummaryProvider;
        let ctx = ToolLoopCostTrackingContext::usage_only();
        let turn_usage = Arc::clone(&ctx.turn_usage);

        let error = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                run_summary(&provider)
                    .await
                    .expect_err("think-only summary cannot be a successful terminal answer")
            })
            .await;

        assert!(
            error
                .chain()
                .any(|cause| cause.is::<SemanticEmptyTerminalCompletion>())
        );
        assert!(
            error
                .to_string()
                .contains("Agent exceeded maximum tool iterations (2)"),
            "the iteration cap remains the caller-visible summary failure: {error}"
        );
        let recorded = *turn_usage.lock();
        assert_eq!(recorded.input_tokens, 100);
        assert_eq!(recorded.output_tokens, 20);
        assert_eq!(recorded.last_input_tokens, 0);
    }

    /// Provider stub that records the exact messages it was dispatched, so a
    /// test can assert on what actually reached the provider.
    struct CapturingProvider {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for CapturingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let joined = request
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            self.seen.lock().unwrap().push(joined);
            Ok(ChatResponse {
                text: Some("wrap-up summary".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl Attributable for CapturingProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "capturing-provider"
        }
    }

    // The graceful-summary path dispatches the accumulated history directly
    // through `run_model_query`, which does NOT run
    // `prepare_messages_for_provider`. A tool-result `[AUDIO:/path]` in that
    // history must be stripped before it reaches the provider, or the raw
    // filesystem path leaks and is hallucinated over on the max-iteration exit.
    #[tokio::test]
    async fn graceful_summary_strips_tool_audio_marker_before_dispatch() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = CapturingProvider {
            seen: Arc::clone(&seen),
        };
        // A properly paired assistant tool_call + native tool-result JSON blob,
        // so the orphaned-tool-message sweep in finish_after_max_iterations keeps
        // the exchange intact and the audio marker survives to dispatch. This
        // also exercises stripping a marker embedded inside a tool-result JSON
        // object (the native-dispatcher shape), not just plain text.
        let mut history = vec![
            ChatMessage::user("call the tool and tell me what you hear"),
            ChatMessage::assistant(r#"{"tool_calls":[{"id":"toolu_1"}]}"#),
            ChatMessage::tool(
                r#"{"content":"[AUDIO:/tmp/clip.wav] recorded 3:00 PM","tool_call_id":"toolu_1"}"#,
            ),
        ];
        let pacing = PacingConfig::default();
        let knobs = LoopKnobs::default();

        let out = finish_after_max_iterations(
            &provider,
            &mut history,
            "custom",
            "test-model",
            None,
            &pacing,
            None,
            2,
            String::new(),
            "trace-req-audio",
            &knobs,
            None,
            None,
        )
        .await
        .expect("graceful summary should succeed");

        assert!(out.contains("wrap-up summary"), "unexpected summary: {out}");
        let captured = seen.lock().unwrap().join("\n");
        assert!(
            !captured.contains("/tmp/clip.wav"),
            "raw audio path reached the provider on the max-iteration path: {captured}"
        );
        assert!(
            captured.contains("[media attachment]"),
            "audio marker should be replaced with a placeholder: {captured}"
        );
    }

    // ACP and other event-driven clients render message content exclusively
    // from `TurnEvent::Chunk`. The max-iteration exit must emit one, and it
    // must carry only the newly-produced segment — narration from earlier
    // iterations already reached the client through prior chunks, so
    // re-sending `accumulated_display_text` here would duplicate it.
    #[tokio::test]
    async fn graceful_summary_emits_a_turn_event_chunk_with_only_the_new_segment() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = CapturingProvider {
            seen: Arc::clone(&seen),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

        let out = run_summary_with_events(&provider, "earlier narration".to_string(), Some(&tx))
            .await
            .expect("graceful summary should succeed");

        assert!(out.contains("wrap-up summary"), "unexpected summary: {out}");

        let mut chunk_delta = None;
        while let Ok(event) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = event {
                chunk_delta = Some(delta);
            }
        }
        let delta = chunk_delta.expect("max-iteration exit must emit a TurnEvent::Chunk");
        assert!(
            delta.contains("wrap-up summary"),
            "chunk must carry the summary text: {delta}"
        );
        assert!(
            delta.contains("Turn stopped: reached maximum tool iterations (2)"),
            "chunk must carry the max-iterations stop reason: {delta}"
        );
        assert!(
            !delta.contains("earlier narration"),
            "chunk must not re-send narration already streamed in earlier iterations: {delta}"
        );
    }

    /// Provider stub returning caller-supplied raw summary text, so a test can
    /// drive the display-hygiene path with think content, terminal markers, or
    /// an internal tool-protocol envelope.
    struct RawTextProvider {
        text: String,
    }

    #[async_trait]
    impl ModelProvider for RawTextProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(self.text.clone())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(self.text.clone()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl Attributable for RawTextProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "raw-text-provider"
        }
    }

    async fn emitted_chunk_for_raw_summary(raw: &str) -> String {
        let provider = RawTextProvider {
            text: raw.to_string(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
        run_summary_with_events(&provider, "earlier narration".to_string(), Some(&tx))
            .await
            .expect("graceful summary should succeed");
        let mut chunk_delta = None;
        while let Ok(event) = rx.try_recv() {
            if let TurnEvent::Chunk { delta } = event {
                chunk_delta = Some(delta);
            }
        }
        chunk_delta.expect("max-iteration exit must emit a TurnEvent::Chunk")
    }

    // The emitted chunk is a live display sink (ACP renders it directly;
    // gateway and RPC forward it unchanged), so the summary must go through
    // the same display-safe contract as the ordinary final response: hidden
    // think content stripped, trailing terminal markers stripped, and an
    // internal tool-protocol envelope suppressed rather than rendered.
    #[tokio::test]
    async fn graceful_summary_chunk_strips_hidden_think_content() {
        let delta =
            emitted_chunk_for_raw_summary("<think>secret chain of thought</think>wrap-up summary")
                .await;
        assert!(
            !delta.contains("secret chain of thought"),
            "hidden think content must not reach the display chunk: {delta}"
        );
        assert!(
            delta.contains("wrap-up summary"),
            "visible summary text must survive: {delta}"
        );
        assert!(
            delta.contains("Turn stopped: reached maximum tool iterations (2)"),
            "chunk must still carry the stop reason: {delta}"
        );
        assert!(
            !delta.contains("earlier narration"),
            "chunk must not re-send earlier narration: {delta}"
        );
    }

    #[tokio::test]
    async fn graceful_summary_chunk_strips_trailing_terminal_marker() {
        let delta = emitted_chunk_for_raw_summary("wrap-up summary<|eom|>").await;
        assert!(
            !delta.contains("eom"),
            "trailing terminal marker must not reach the display chunk: {delta}"
        );
        assert!(
            delta.contains("wrap-up summary"),
            "visible summary text must survive: {delta}"
        );
        assert!(
            delta.contains("Turn stopped: reached maximum tool iterations (2)"),
            "chunk must still carry the stop reason: {delta}"
        );
    }

    #[tokio::test]
    async fn graceful_summary_chunk_suppresses_internal_tool_protocol_envelope() {
        let delta = emitted_chunk_for_raw_summary(
            "<tool_call>{\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}</tool_call>",
        )
        .await;
        assert!(
            !delta.contains("tool_call"),
            "internal tool-protocol envelope must not be rendered: {delta}"
        );
        assert!(
            !delta.contains("shell"),
            "internal tool-protocol payload must not be rendered: {delta}"
        );
        assert!(
            delta.contains("internal tool-call format error"),
            "suppressed protocol output must fall back to the safe notice: {delta}"
        );
        assert!(
            delta.contains("Turn stopped: reached maximum tool iterations (2)"),
            "chunk must still carry the stop reason: {delta}"
        );
        assert!(
            !delta.contains("earlier narration"),
            "chunk must not re-send earlier narration: {delta}"
        );
    }
}

#[cfg(test)]
mod i18n_message_tests {
    /// The graceful max-iteration shutdown must include the iteration count in
    /// the user-visible message so the operator knows why the agent stopped.
    #[test]
    fn max_iterations_message_includes_count() {
        let msg = crate::i18n::get_required_cli_string_with_args(
            "turn-max-iterations-reached",
            &[("max_iterations", "42")],
        );
        assert!(
            msg.contains("42"),
            "message should contain iteration count: {msg}"
        );
        assert!(
            msg.contains("maximum tool iterations"),
            "message should describe the limit: {msg}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::turn_stop::turn_stop;

    #[test]
    fn the_iteration_cap_stop_is_typed_and_says_what_it_always_said() {
        let stop = max_iterations_stop(10);
        assert_eq!(stop.code, TurnStopCode::MaxIterations);
        assert_eq!(
            stop.to_string(),
            "Agent exceeded maximum tool iterations (10)"
        );
        let err: anyhow::Error = stop.into();
        assert_eq!(
            turn_stop(&err)
                .expect("stop must survive the anyhow hop")
                .code,
            TurnStopCode::MaxIterations
        );
    }
}
