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
struct ReplayState {
    turns: Vec<VecDeque<TraceResponse>>,
    current: usize,
}

/// Replays a trace's scripted steps with the turn boundary preserved.
///
/// Each turn keeps its own FIFO queue and `chat()` pops only from the queue of the
/// turn currently being replayed, so a turn can never consume a neighbouring turn's
/// scripted responses. Requesting more responses than the *current turn* scripts is
/// an error (exhaustion guard); leaving a turn's steps unconsumed is caught by
/// [`ReplayHandle::finish_turn`], which the runner calls at every turn boundary.
pub struct TraceLlmProvider {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl TraceLlmProvider {
    /// Build a replay provider from a trace, keeping each turn's scripted steps in its
    /// own queue. Fails if any turn has no scripted steps: replay requires every LLM
    /// round-trip to be scripted, so an empty turn is an authoring error rather than a
    /// live case.
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
/// Shares the provider's queues (the same `Arc` the agent's provider holds), so the
/// runner can assert per-turn consumption without owning the boxed provider.
#[derive(Clone)]
pub struct ReplayHandle {
    state: Arc<Mutex<ReplayState>>,
    trace_name: String,
}

impl ReplayHandle {
    /// Assert the just-finished turn consumed all of its scripted steps, then advance
    /// the cursor to the next turn. Errors if any steps were left unconsumed — an
    /// over-specified turn would otherwise bleed its leftovers into the next turn.
    pub fn finish_turn(&self, turn_index: usize) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let leftover = state.turns.get(state.current).map_or(0, |q| q.len());
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

    /// Two turns, one scripted step each, with distinguishable content.
    const TWO_TURNS: &str = r#"{
        "model_name": "test-two-turns",
        "turns": [
            { "user_input": "one", "steps": [{ "response": { "type": "text", "content": "TURN-ONE" } }] },
            { "user_input": "two", "steps": [{ "response": { "type": "text", "content": "TURN-TWO" } }] }
        ],
        "expects": {}
    }"#;

    fn provider(json: &str) -> TraceLlmProvider {
        let trace: LlmTrace = serde_json::from_str(json).unwrap();
        TraceLlmProvider::try_from_trace(&trace).unwrap()
    }

    async fn chat_text(p: &TraceLlmProvider) -> anyhow::Result<String> {
        let request = ChatRequest {
            messages: &[],
            tools: None,
            thinking: None,
        };
        Ok(p.chat(request, "m", None).await?.text.unwrap_or_default())
    }

    #[tokio::test]
    async fn finish_turn_rejects_unconsumed_steps() {
        // Turn 0 scripts two steps but only one is requested, so a step is left
        // unconsumed. The turn boundary must reject it rather than carry it forward.
        let p = provider(
            r#"{
                "model_name": "test-leftover",
                "turns": [
                    { "user_input": "one", "steps": [
                        { "response": { "type": "text", "content": "used" } },
                        { "response": { "type": "text", "content": "leftover" } }
                    ] },
                    { "user_input": "two", "steps": [{ "response": { "type": "text", "content": "TURN-TWO" } }] }
                ],
                "expects": {}
            }"#,
        );
        let handle = p.handle();
        assert_eq!(chat_text(&p).await.unwrap(), "used");

        let err = handle
            .finish_turn(0)
            .expect_err("turn 0 left a scripted step unconsumed and must not be accepted");
        let msg = err.to_string();
        assert!(
            msg.contains("test-leftover"),
            "error must name the trace: {msg}"
        );
        assert!(
            msg.contains("turn 0"),
            "error must name the offending turn: {msg}"
        );
        assert!(
            msg.contains("1 step(s)"),
            "error must report the leftover count: {msg}"
        );
    }

    #[tokio::test]
    async fn finish_turn_accepts_a_fully_consumed_turn() {
        let p = provider(TWO_TURNS);
        let handle = p.handle();
        assert_eq!(chat_text(&p).await.unwrap(), "TURN-ONE");
        handle.finish_turn(0).expect("turn 0 consumed every step");
        assert_eq!(chat_text(&p).await.unwrap(), "TURN-TWO");
        handle.finish_turn(1).expect("turn 1 consumed every step");
    }

    #[tokio::test]
    async fn an_unconsumed_turn_never_releases_the_next_turns_steps() {
        // Turn 0 over-specifies. `finish_turn` refuses to advance the cursor, so the
        // replay is pinned inside turn 0: turn 1's scripted step stays unreachable and
        // can never be silently attributed to turn 0's conversation.
        let p = provider(
            r#"{
                "model_name": "test-bleed",
                "turns": [
                    { "user_input": "one", "steps": [
                        { "response": { "type": "text", "content": "TURN-ONE" } },
                        { "response": { "type": "text", "content": "LEFTOVER-FROM-TURN-ONE" } }
                    ] },
                    { "user_input": "two", "steps": [{ "response": { "type": "text", "content": "TURN-TWO" } }] }
                ],
                "expects": {}
            }"#,
        );
        let handle = p.handle();
        assert_eq!(chat_text(&p).await.unwrap(), "TURN-ONE");
        assert!(handle.finish_turn(0).is_err());

        // A runner that swallowed the boundary error still cannot reach turn 1's step.
        let next = chat_text(&p).await.unwrap();
        assert_eq!(
            next, "LEFTOVER-FROM-TURN-ONE",
            "the cursor must stay on the unconsumed turn"
        );
        assert_ne!(
            next, "TURN-TWO",
            "turn 1's scripted step must not be served while turn 0 is unfinished"
        );
    }

    #[tokio::test]
    async fn exhausting_a_turn_does_not_fall_through_to_the_next_turn() {
        // Turn 0 scripts one step; a second request within turn 0 must error rather
        // than silently pop turn 1's response.
        let p = provider(TWO_TURNS);
        assert_eq!(chat_text(&p).await.unwrap(), "TURN-ONE");
        let err = chat_text(&p)
            .await
            .expect_err("a second request in turn 0 must not borrow turn 1's scripted step");
        let msg = err.to_string();
        assert!(
            msg.contains("more LLM responses than the trace provides for that turn"),
            "error must be turn-scoped: {msg}"
        );
        assert!(msg.contains("turn 0"), "error must name the turn: {msg}");
    }
}
