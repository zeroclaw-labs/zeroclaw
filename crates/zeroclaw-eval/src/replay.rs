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
    pub fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        // The error message below is labeled with the caller-supplied
        // `turn_index`, but the leftover check itself reads `state.current`.
        // These must always agree (the runner calls `finish_turn` once per
        // turn, in order): harden against a future out-of-order or
        // skipped-turn caller silently mislabeling the error.
        debug_assert_eq!(
            turn_index, state.current,
            "finish_turn called out of order: caller says turn {turn_index}, replay cursor is at turn {}",
            state.current
        );
        let leftover = state.turns.get(state.current).map_or(0, |q| q.len());
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
