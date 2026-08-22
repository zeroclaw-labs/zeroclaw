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
/// the turn currently being replayed. Turn boundaries are enforced: a turn may
/// only consume its own steps, and it must consume all of them.
struct ReplayState {
    turns: Vec<VecDeque<TraceResponse>>,
    current: usize,
}

/// Replays a trace's scripted steps, one queue per turn.
///
/// The provider is opaque to the runner (it is injected as a boxed `ModelProvider`
/// through `RunDeps`), so the runner drives turn boundaries through the
/// [`ReplayHandle`] returned alongside it. Requesting more responses than the
/// current turn scripts is an error (exhaustion guard); leaving steps unconsumed
/// at the end of a turn is an error too (over-specification guard).
pub struct TraceLlmProvider {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl TraceLlmProvider {
    /// Build a replay provider from a trace, keeping each turn's scripted steps in
    /// its own FIFO queue. Fails if any turn has no scripted steps: replay requires
    /// every LLM round-trip to be scripted, so an empty turn is an authoring error
    /// rather than a live case.
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
#[derive(Clone)]
pub struct ReplayHandle {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl ReplayHandle {
    /// Assert the just-finished turn consumed all of its scripted steps, then advance
    /// the cursor to the next turn. Errors if any steps were left unconsumed — including
    /// on the final turn, where flattened replay would otherwise ignore them silently.
    pub fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let leftover = state.turns.get(state.current).map_or(0, VecDeque::len);
        if leftover > 0 {
            anyhow::bail!(
                "TraceLlmProvider({}): turn {turn_index} scripted {leftover} step(s) the agent never requested — the trace over-specifies this turn's LLM round-trips",
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
        // Not used by the agent loop (which uses `chat`); used to script the judge
        // in tests: pop the next step of the current turn, which must be a Text step.
        let step = {
            let mut state = self.state.lock().unwrap();
            let current = state.current;
            state.turns.get_mut(current).and_then(VecDeque::pop_front)
        };
        match step {
            Some(TraceResponse::Text { content, .. }) => Ok(content),
            Some(TraceResponse::ToolCalls { .. }) => {
                anyhow::bail!(
                    "TraceLlmProvider({}): chat_with_system got a tool_calls step; scripted judge responses must be text",
                    self.trace_name
                )
            }
            None => anyhow::bail!(
                "TraceLlmProvider({}): chat_with_system requested more responses than scripted",
                self.trace_name
            ),
        }
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
            match state.turns.get_mut(current).and_then(VecDeque::pop_front) {
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
