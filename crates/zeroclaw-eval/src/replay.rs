//! A [`ModelProvider`] that replays scripted LLM responses from an [`LlmTrace`].
//! Promoted from the test-only trace-replay helper so the same deterministic
//! engine backs both the shipped `zeroclaw eval` command and the test suite.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::model_provider::{
    ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities, TokenUsage, ToolCall,
};

use crate::case::{LlmTrace, TraceResponse};

/// One FIFO queue of scripted steps per conversation turn, plus a cursor marking
/// the turn currently being replayed. Keeping steps per-turn (rather than one
/// flattened queue) means a turn can only ever consume its own scripted steps:
/// it cannot under-run into the next turn's responses, and leftover steps at
/// turn end are a detectable authoring error (see `ReplayHandle::finish_turn`).
struct ReplayState {
    turns: Vec<VecDeque<TraceResponse>>,
    current: usize,
}

/// Replays a trace's scripted steps in FIFO order, one queue per conversation turn.
///
/// The provider is opaque to the runner (it is injected as a boxed `ModelProvider`
/// through `RunDeps`), so turn boundaries are enforced via the companion
/// [`ReplayHandle`] rather than by the provider alone: the runner calls
/// `ReplayHandle::finish_turn` after each `Agent::turn` completes to assert the
/// turn's scripted steps were fully consumed and advance the cursor. Requesting
/// more responses than a turn scripts is an error (per-turn exhaustion guard).
pub struct TraceLlmProvider {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl TraceLlmProvider {
    /// Build a replay provider from a trace, keeping each turn's steps in its own
    /// queue. Fails if any turn has no scripted steps: replay requires every LLM
    /// round-trip to be scripted, so an empty turn is an authoring error rather
    /// than a live case.
    pub fn try_from_trace(trace: &LlmTrace) -> anyhow::Result<Self> {
        let mut turns = Vec::with_capacity(trace.turns.len());
        for (turn_index, turn) in trace.turns.iter().enumerate() {
            let turn_steps = turn.steps.as_deref().unwrap_or_default();
            if turn_steps.is_empty() {
                anyhow::bail!(
                    "replay case '{}' turn {} has no scripted steps",
                    trace.model_name,
                    turn_index
                );
            }
            turns.push(turn_steps.iter().map(|s| s.response.clone()).collect());
        }
        Ok(Self {
            state: Arc::new(Mutex::new(ReplayState { turns, current: 0 })),
            trace_name: trace.model_name.clone(),
        })
    }

    /// A handle the runner uses to advance turn boundaries while it drives the agent.
    pub fn handle(&self) -> ReplayHandle {
        ReplayHandle {
            state: Arc::clone(&self.state),
            trace_name: self.trace_name.clone(),
        }
    }
}

/// Runner-side handle for advancing the replay cursor between conversation turns.
/// Shares the provider's queues (the same `Arc` the agent holds), so the runner can
/// assert per-turn consumption without owning the boxed provider.
pub struct ReplayHandle {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl ReplayHandle {
    /// Assert the just-finished turn consumed all of its scripted steps, then advance
    /// the cursor to the next turn. Errors if any steps were left unconsumed.
    ///
    /// Both cursor-integrity failures below return a real error rather than a
    /// `debug_assert!`. A `debug_assert!` is compiled out of release builds,
    /// which is exactly where an eval report is used as evidence: a caller
    /// that skipped or mislabeled a turn would otherwise advance the cursor
    /// and return `Ok(())`, certifying a replay that never checked the turn
    /// it claims to have checked.
    pub fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        // The leftover check below reads `state.current`, while the error
        // message is labeled with the caller-supplied `turn_index`. These
        // must always agree (the runner calls `finish_turn` once per turn, in
        // order); a mismatch means the caller skipped or reordered a turn, so
        // whatever this call would verify is not the turn it names.
        if turn_index != state.current {
            anyhow::bail!(
                "TraceLlmProvider({}): finish_turn called out of order: caller says turn {turn_index}, replay cursor is at turn {} - a skipped or mislabeled turn would certify a replay boundary that was never checked",
                self.trace_name,
                state.current
            );
        }
        // An exhausted or out-of-range cursor has no queue to inspect.
        // Treating that as "zero steps left over" would silently pass the
        // per-turn contract for a turn the trace never scripted.
        let Some(queue) = state.turns.get(state.current) else {
            anyhow::bail!(
                "TraceLlmProvider({}): finish_turn called for turn {turn_index}, but the trace only scripts {} turn(s) - the replay cursor is exhausted",
                self.trace_name,
                state.turns.len()
            );
        };
        let leftover = queue.len();
        if leftover > 0 {
            anyhow::bail!(
                "TraceLlmProvider({}): turn {turn_index} scripted {leftover} step(s) the agent never requested - the trace over-specifies this turn's LLM round-trips",
                self.trace_name
            );
        }
        state.current += 1;
        Ok(())
    }
}

impl Attributable for TraceLlmProvider {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
    }

    fn alias(&self) -> &str {
        "eval-replay"
    }
}

#[async_trait]
impl ModelProvider for TraceLlmProvider {
    /// Truthful capabilities so the provider stays correct if ever routed through
    /// dispatcher resolution (`tool_dispatcher_for_provider`): the scripted tool
    /// calls are native tool calls.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            ..ProviderCapabilities::default()
        }
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        // Not exercised by the agent loop (which uses `chat`); kept for trait completeness.
        Ok(String::new())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let step = {
            let mut state = self.state.lock().unwrap();
            let current = state.current;
            match state.turns.get_mut(current).and_then(|q| q.pop_front()) {
                Some(step) => step,
                None => anyhow::bail!(
                    "TraceLlmProvider({}): turn {current} requested more LLM responses than the trace provides for that turn",
                    self.trace_name
                ),
            }
        };
        match step {
            TraceResponse::Text {
                content,
                input_tokens,
                output_tokens,
            } => Ok(ChatResponse {
                text: Some(content),
                tool_calls: vec![],
                usage: Some(TokenUsage {
                    input_tokens: Some(input_tokens),
                    output_tokens: Some(output_tokens),
                    cached_input_tokens: None,
                }),
                reasoning_content: None,
            }),
            TraceResponse::ToolCalls {
                tool_calls,
                input_tokens,
                output_tokens,
            } => {
                let calls = tool_calls
                    .into_iter()
                    .map(|tc| ToolCall {
                        id: tc.id,
                        name: tc.name,
                        arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                        extra_content: None,
                    })
                    .collect();
                Ok(ChatResponse {
                    text: Some(String::new()),
                    tool_calls: calls,
                    usage: Some(TokenUsage {
                        input_tokens: Some(input_tokens),
                        output_tokens: Some(output_tokens),
                        cached_input_tokens: None,
                    }),
                    reasoning_content: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two turns, one scripted step each, so a cursor at 0 or 1 is in range
    /// and a cursor at 2 is exhausted.
    fn two_turn_trace() -> LlmTrace {
        serde_json::from_str(
            r#"{"model_name":"cursor","turns":[
                {"user_input":"a","steps":[{"response":{"type":"text","content":"1"}}]},
                {"user_input":"b","steps":[{"response":{"type":"text","content":"2"}}]}
            ]}"#,
        )
        .unwrap()
    }

    async fn consume_one_step(provider: &TraceLlmProvider) {
        provider
            .chat(
                ChatRequest {
                    messages: &[],
                    tools: None,
                    thinking: None,
                },
                "cursor",
                None,
            )
            .await
            .expect("the trace scripts a step for this turn");
    }

    #[tokio::test]
    async fn finish_turn_rejects_a_mislabeled_turn_index() {
        // A caller that names a turn other than the one the cursor is on is
        // verifying a different turn than the one it reports. This used to be
        // a `debug_assert_eq!`, which is compiled out in release: the call
        // would advance the cursor and return `Ok(())`, certifying a boundary
        // that was never actually checked.
        let provider = TraceLlmProvider::try_from_trace(&two_turn_trace()).unwrap();
        let handle = provider.handle();
        consume_one_step(&provider).await;

        let err = handle
            .finish_turn(1)
            .expect_err("cursor is at turn 0, so a caller naming turn 1 must be rejected");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("out of order"),
            "error must identify the out-of-order call, got: {rendered}"
        );
        assert!(
            rendered.contains("caller says turn 1") && rendered.contains("cursor is at turn 0"),
            "error must report both the claimed and actual turn, got: {rendered}"
        );

        // The rejected call must not have advanced the cursor: the correctly
        // labeled call still succeeds afterwards.
        handle
            .finish_turn(0)
            .expect("a rejected out-of-order call must not move the cursor");
    }

    #[tokio::test]
    async fn finish_turn_rejects_an_exhausted_cursor() {
        // Past the end of the trace there is no queue to inspect. The old
        // `map_or(0, ..)` read that as "zero steps left over" and returned
        // `Ok(())`, so a runner could claim a per-turn contract for a turn the
        // trace never scripted.
        let provider = TraceLlmProvider::try_from_trace(&two_turn_trace()).unwrap();
        let handle = provider.handle();
        consume_one_step(&provider).await;
        handle.finish_turn(0).unwrap();
        consume_one_step(&provider).await;
        handle.finish_turn(1).unwrap();

        let err = handle
            .finish_turn(2)
            .expect_err("the trace scripts only 2 turns, so turn 2 must be rejected");
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("exhausted"),
            "error must identify the exhausted cursor, got: {rendered}"
        );
        assert!(
            rendered.contains("only scripts 2 turn(s)"),
            "error must report how many turns the trace scripts, got: {rendered}"
        );
    }

    #[tokio::test]
    async fn finish_turn_still_rejects_an_overspecified_turn() {
        // The pre-existing leftover-steps contract must survive the rewrite.
        let trace: LlmTrace = serde_json::from_str(
            r#"{"model_name":"leftover","turns":[
                {"user_input":"a","steps":[
                    {"response":{"type":"text","content":"1"}},
                    {"response":{"type":"text","content":"never requested"}}
                ]}
            ]}"#,
        )
        .unwrap();
        let provider = TraceLlmProvider::try_from_trace(&trace).unwrap();
        let handle = provider.handle();
        consume_one_step(&provider).await;

        let err = handle
            .finish_turn(0)
            .expect_err("one scripted step was never requested");
        assert!(
            format!("{err:#}").contains("over-specifies"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn finish_turn_accepts_a_fully_consumed_turn() {
        let provider = TraceLlmProvider::try_from_trace(&two_turn_trace()).unwrap();
        let handle = provider.handle();
        consume_one_step(&provider).await;
        handle.finish_turn(0).unwrap();
        consume_one_step(&provider).await;
        handle.finish_turn(1).unwrap();
    }
}
