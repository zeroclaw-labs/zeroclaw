//! The max-iteration exit: when the loop exhausts its iterations, ask the
//! LLM for a tools-free final summary (with step timeout + cancel select)
//! and return it appended to the accumulated display text, or bail.

use super::knobs::{LoopKnobs, MaxIterationBehavior};
use super::outcome::ToolLoopCancelled;
use anyhow::Result;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::PacingConfig;
use zeroclaw_providers::{ChatMessage, ModelProvider};

/// Graceful shutdown after the loop exhausts `max_iterations` (upstream loop
/// body, max-iteration exit): log exhaustion, push a summary-request user
/// message, make a tools-free `chat` call honoring `pacing.step_timeout_secs`
/// and the cancellation token, and return `Ok(accumulated + summary)` — or
/// bail with "exceeded maximum tool iterations" when the summary is empty or
/// the call fails.
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
        anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
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
    // Sanitise tool_use / tool_result pairing before the graceful-shutdown
    // request. When the loop exits immediately after the model emits a
    // tool_use (hitting max_tool_iterations before the runner records a
    // tool_result), the history carries an unpaired tool_use block.
    // Bedrock/Anthropic reject the follow-up tools-free summary call with:
    // "Expected toolResult blocks at messages.N.content for the following
    // Ids: tooluse_*". Two complementary sweeps:
    //   1. strip_orphaned_tool_calls_from_assistants — removes tool_calls from
    //      assistant messages whose ids have no following tool result.
    //   2. remove_orphaned_tool_messages — removes tool-role messages that no
    //      longer have a matching assistant (symmetric case).
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
    // Pushed into history for the request below, but mirrored into the
    // append-log (and kept in history) only when the summary call SUCCEEDS:
    // a failed/cancelled/timed-out/empty summary must not persist an
    // unanswered synthetic prompt into wrapper transcripts — every failure
    // exit pops it back off.
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
            anyhow::bail!("Final summary LLM call timed out after {step_secs}s (step_timeout_secs)")
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
            anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
        }
        SummaryCall::Done(Ok(resp)) => resp,
    };

    let text = resp.text.unwrap_or_default();
    if text.is_empty() {
        history.pop();
        anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
    }
    // Persist the answered prompt + summary like every other final assistant
    // response: without the summary message, persistent-history callers (the
    // streamed wrapper's replay, new_messages consumers) store a transcript
    // ending on the synthetic user prompt with no answer — the delivered
    // summary would be absent and the model re-answers the synthetic prompt
    // next turn.
    let summary_msg = ChatMessage::assistant(text.clone());
    if let Some(out) = &mut new_messages_out {
        out.push(summary_prompt_mirror);
        out.push(summary_msg.clone());
    }
    history.push(summary_msg);
    accumulated_display_text.push_str(&text);
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
    use zeroclaw_api::model_provider::{ChatRequest, ChatResponse};
    use zeroclaw_config::schema::{CostConfig, PacingConfig};
    use zeroclaw_providers::traits::TokenUsage;
    use zeroclaw_providers::{ChatMessage, ModelProvider};

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

    async fn run_summary(provider: &CountingUsageProvider) -> anyhow::Result<String> {
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
            String::new(),
            "trace-req-test",
            &knobs,
            None,
        )
        .await
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
}
