//! Shared mock provider implementations for integration tests.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use zeroclaw::providers::traits::{ChatMessage, TokenUsage};
use zeroclaw::providers::{ChatRequest, ChatResponse, Provider, ToolCall};

use super::trace::{LlmTrace, TraceResponse};

/// Mock provider that returns scripted responses in FIFO order.
pub struct MockProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl MockProvider {
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok("fallback".into());
        }
        let resp = guard.remove(0);
        Ok(resp.text.unwrap_or_else(|| "fallback".into()))
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}

/// Mock provider that returns scripted responses AND records every request.
pub struct RecordingProvider {
    responses: Mutex<Vec<ChatResponse>>,
    recorded_requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

impl RecordingProvider {
    pub fn new(responses: Vec<ChatResponse>) -> (Self, Arc<Mutex<Vec<Vec<ChatMessage>>>>) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let provider = Self {
            responses: Mutex::new(responses),
            recorded_requests: recorded.clone(),
        };
        (provider, recorded)
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        self.recorded_requests
            .lock()
            .unwrap()
            .push(request.messages.to_vec());

        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}

/// Provider that replays responses from an `LlmTrace` fixture.
///
/// Each call to `chat()` returns the next step from the trace in FIFO order.
/// If the agent calls the provider more times than there are steps, an error is returned.
pub struct TraceLlmProvider {
    steps: Mutex<Vec<TraceResponse>>,
    trace_name: String,
}

impl TraceLlmProvider {
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

#[async_trait]
impl Provider for TraceLlmProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut guard = self.steps.lock().unwrap();
        if guard.is_empty() {
            anyhow::bail!(
                "TraceLlmProvider({}) exhausted: no more steps in trace",
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
