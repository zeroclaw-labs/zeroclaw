//! A [`ModelProvider`] that replays scripted LLM responses from an [`LlmTrace`].
//! Promoted from the test-only trace-replay helper so the same deterministic
//! engine backs both the shipped `zeroclaw eval` command and the test suite.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::model_provider::{
    ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities, TokenUsage, ToolCall,
};

use crate::case::{LlmTrace, TraceResponse};

/// Replays a trace's scripted steps in FIFO order across the whole conversation.
///
/// The provider is opaque to the runner (it is injected as a boxed `ModelProvider`
/// through `RunDeps`), so turn boundaries are not enforced externally; the trace's
/// steps are flattened into a single queue and consumed in order. Requesting more
/// responses than the trace scripts is an error (exhaustion guard).
pub struct TraceLlmProvider {
    steps: Mutex<VecDeque<TraceResponse>>,
    trace_name: String,
}

impl TraceLlmProvider {
    /// Build a replay provider from a trace, flattening every turn's scripted steps
    /// into one FIFO queue. Fails if any turn has no scripted steps: replay requires
    /// every LLM round-trip to be scripted, so an empty turn is an authoring error
    /// rather than a live case.
    pub fn try_from_trace(trace: &LlmTrace) -> anyhow::Result<Self> {
        let mut steps = VecDeque::new();
        for (turn_index, turn) in trace.turns.iter().enumerate() {
            let turn_steps = turn.steps.as_deref().unwrap_or_default();
            if turn_steps.is_empty() {
                anyhow::bail!(
                    "replay case '{}' turn {} has no scripted steps",
                    trace.model_name,
                    turn_index
                );
            }
            for step in turn_steps {
                steps.push_back(step.response.clone());
            }
        }
        Ok(Self {
            steps: Mutex::new(steps),
            trace_name: trace.model_name.clone(),
        })
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
        // in tests: pop the next step, which must be a Text step, and return it.
        let step = {
            let mut steps = self.steps.lock().unwrap();
            steps.pop_front()
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
            let mut steps = self.steps.lock().unwrap();
            match steps.pop_front() {
                Some(step) => step,
                None => anyhow::bail!(
                    "TraceLlmProvider({}): the agent requested more LLM responses than the trace provides",
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
