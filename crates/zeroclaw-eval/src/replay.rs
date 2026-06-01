//! A [`ModelProvider`] that replays scripted LLM responses from an [`LlmTrace`].
//!
//! Promoted from the test-only trace-replay helper so the same deterministic
//! engine backs both the shipped `zeroclaw eval` command and the test suite.

use async_trait::async_trait;
use std::sync::Mutex;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::model_provider::{
    ChatRequest, ChatResponse, ModelProvider, TokenUsage, ToolCall,
};

use crate::case::{LlmTrace, TraceResponse};

/// Replays the steps of an [`LlmTrace`] in FIFO order.
///
/// Each call to [`ModelProvider::chat`] returns the next step. All steps across
/// all turns are flattened into a single queue (matching how the agent loop pulls
/// responses), and exhausting the queue is an error — this surfaces a trace that
/// under-specifies the number of LLM round-trips the agent actually makes.
pub struct TraceLlmProvider {
    steps: Mutex<Vec<TraceResponse>>,
    trace_name: String,
}

impl TraceLlmProvider {
    /// Build a replay provider from a trace, flattening every step in order.
    pub fn from_trace(trace: &LlmTrace) -> Self {
        let mut steps = Vec::new();
        for turn in &trace.turns {
            for step in &turn.steps {
                steps.push(step.response.clone());
            }
        }
        Self {
            steps: Mutex::new(steps),
            trace_name: trace.model_name.clone(),
        }
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
        let mut guard = self.steps.lock().unwrap();
        if guard.is_empty() {
            anyhow::bail!(
                "TraceLlmProvider({}) exhausted: the agent requested more LLM responses than the trace provides",
                self.trace_name
            );
        }
        let step = guard.remove(0);
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
