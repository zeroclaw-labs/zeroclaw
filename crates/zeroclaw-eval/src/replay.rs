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
/// the turn currently being replayed.
///
/// The queues are deliberately *not* flattened into a single FIFO. A flat queue
/// lets a turn that over-specifies its LLM round-trips bleed the surplus response
/// into the next turn, and the case still passes — a replay suite used as a merge
/// gate would then certify behaviour that never happened.
struct ReplayState {
    turns: Vec<VecDeque<TraceResponse>>,
    current: usize,
}

/// Replays a trace's scripted steps, one queue per turn, in order.
///
/// The provider is injected into the runner as a boxed `ModelProvider`, so the
/// runner drives turn boundaries through a [`ReplayHandle`] that shares these
/// queues. Requesting more responses than the trace scripts *for the current turn*
/// is an error (exhaustion guard), as is leaving any scripted for it unconsumed
/// (over-specification guard, see [`ReplayHandle::finish_turn`]).
pub struct TraceLlmProvider {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl TraceLlmProvider {
    /// Build a replay provider from a trace, keeping each turn's scripted steps in
    /// its own queue. Fails if any turn has no scripted steps: replay requires every
    /// LLM round-trip to be scripted, so an empty turn is an authoring error rather
    /// than a live case.
    pub fn try_from_trace(trace: &LlmTrace) -> anyhow::Result<Self> {
        let mut turns: Vec<VecDeque<TraceResponse>> = Vec::with_capacity(trace.turns.len());
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

impl crate::runner::TurnBoundary for ReplayHandle {
    fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()> {
        ReplayHandle::finish_turn(self, turn_index)
    }
}

impl ReplayHandle {
    /// Assert the just-finished turn consumed all of its scripted steps, then advance
    /// the cursor to the next turn. Errors if any steps were left unconsumed, so an
    /// over-specified turn fails the case instead of leaking into the next one.
    pub fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let current = state.current;
        let leftover = state.turns.get(current).map_or(0, |q| q.len());
        anyhow::ensure!(
            leftover == 0,
            "TraceLlmProvider({}): turn {turn_index} scripted {leftover} step(s) the agent never requested — the trace over-specifies this turn's LLM round-trips",
            self.trace_name
        );
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
