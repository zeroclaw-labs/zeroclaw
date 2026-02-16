//! Reusable agentic executor — runs a multi-turn tool-use loop against an LLM.
//!
//! This is the core "brain" of Aria. Given a provider, tools, system prompt,
//! and user input, it runs the agent loop:
//!   1. Send messages + tool specs to the LLM
//!   2. If response contains `tool_use` blocks → execute tools → append results
//!   3. Loop until the LLM emits a final text response (no tool calls)
//!   4. Return the final text and execution metadata

use crate::providers::{
    ChatCompletionResponse, ChatContent, ChatMessage, ContentBlock, Provider, ProviderStreamSink,
    ToolDefinition,
};
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionEnvironment {
    Dev,
    Prod,
}

impl ToolExecutionEnvironment {
    pub fn from_env() -> Self {
        let raw = std::env::var("ARIA_ENVIRONMENT")
            .or_else(|_| std::env::var("AFW_ENV"))
            .unwrap_or_else(|_| "dev".to_string());
        match raw.trim().to_ascii_lowercase().as_str() {
            "prod" | "production" => Self::Prod,
            _ => Self::Dev,
        }
    }

    fn block_local_fallback(self, tool_name: &str, external_execution_handled: bool) -> bool {
        self == Self::Prod && is_local_machine_tool(tool_name) && !external_execution_handled
    }
}

fn is_local_machine_tool(name: &str) -> bool {
    matches!(name, "shell" | "file_read" | "file_write")
}

/// Maximum agentic turns (LLM calls) before forcing stop.
const DEFAULT_MAX_TURNS: u32 = 25;
/// Default `max_tokens` per LLM call.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Result of an agent execution run.
#[derive(Debug, Clone)]
pub struct AgentExecutionResult {
    /// Final text output from the agent.
    pub output: String,
    /// Whether execution completed successfully.
    pub success: bool,
    /// Number of LLM turns taken.
    pub turns: u32,
    /// Number of tool calls executed.
    pub tool_calls: u32,
    /// Total duration in milliseconds.
    pub duration_ms: u64,
    /// Error message if failed.
    pub error: Option<String>,
    /// Tool execution traces (args/results/durations) for observability.
    pub tool_traces: Vec<AgentToolTrace>,
}

#[derive(Debug, Clone)]
pub struct AgentToolTrace {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
    pub result: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ExternalToolCall {
    pub tenant_id: String,
    pub chat_id: String,
    pub run_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ExternalToolResult {
    pub output: String,
    pub is_error: bool,
}

#[async_trait]
pub trait ExternalToolExecutor: Send + Sync {
    async fn execute_external_tool(
        &self,
        call: &ExternalToolCall,
    ) -> Result<Option<ExternalToolResult>>;
}

#[derive(Clone)]
pub struct ExternalToolContext {
    pub tenant_id: String,
    pub chat_id: String,
    pub run_id: String,
    pub executor: Arc<dyn ExternalToolExecutor>,
}

#[async_trait]
pub trait AgentExecutionSink: Send {
    async fn on_assistant_delta(&mut self, _delta: &str, _accumulated: &str) {}
    async fn on_thinking_start(&mut self) {}
    async fn on_thinking_delta(&mut self, _delta: &str) {}
    async fn on_thinking_end(&mut self) {}
    async fn on_tool_start(&mut self, _id: &str, _name: &str, _args: &serde_json::Value) {}
    async fn on_tool_end(
        &mut self,
        _id: &str,
        _name: &str,
        _result: &str,
        _is_error: bool,
        _duration_ms: u64,
    ) {
    }
}

/// Convert a Tool trait object to a `ToolDefinition` for the LLM.
fn tool_to_definition(tool: &dyn Tool) -> ToolDefinition {
    ToolDefinition {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        input_schema: tool.parameters_schema(),
    }
}

/// Execute an agentic loop: send message to LLM, execute tool calls, repeat.
///
/// # Arguments
/// * `provider` - The LLM provider to use
/// * `tools` - Available tools the agent can call
/// * `system_prompt` - System prompt defining agent behavior
/// * `user_input` - The user's message/task
/// * `model` - Model identifier
/// * `temperature` - Sampling temperature
/// * `max_turns` - Maximum number of LLM round-trips (None = default 25)
pub async fn execute_agent(
    provider: &dyn Provider,
    tools: &[Box<dyn Tool>],
    system_prompt: &str,
    user_input: &str,
    model: &str,
    temperature: f64,
    max_turns: Option<u32>,
) -> Result<AgentExecutionResult> {
    execute_agent_with_sink(
        provider,
        tools,
        system_prompt,
        user_input,
        model,
        temperature,
        max_turns,
        None,
        None,
    )
    .await
}

pub async fn execute_agent_with_sink(
    provider: &dyn Provider,
    tools: &[Box<dyn Tool>],
    system_prompt: &str,
    user_input: &str,
    model: &str,
    temperature: f64,
    max_turns: Option<u32>,
    external_tool_context: Option<ExternalToolContext>,
    mut sink: Option<&mut dyn AgentExecutionSink>,
) -> Result<AgentExecutionResult> {
    let start = Instant::now();
    let max = max_turns.unwrap_or(DEFAULT_MAX_TURNS);
    let execution_environment = ToolExecutionEnvironment::from_env();

    // Build tool definitions for the LLM
    let tool_defs: Vec<ToolDefinition> = tools
        .iter()
        .map(|t| tool_to_definition(t.as_ref()))
        .collect();

    // Build initial conversation
    let mut messages: Vec<ChatMessage> = vec![ChatMessage {
        role: "user".into(),
        content: ChatContent::Text(user_input.to_string()),
    }];

    let mut total_turns: u32 = 0;
    let mut total_tool_calls: u32 = 0;
    let mut tool_traces: Vec<AgentToolTrace> = Vec::new();
    let mut accumulated_assistant = String::new();

    struct ExecutionProviderSink<'a> {
        sink: &'a mut dyn AgentExecutionSink,
        accumulated: &'a mut String,
    }

    #[async_trait]
    impl ProviderStreamSink for ExecutionProviderSink<'_> {
        async fn on_assistant_delta(&mut self, delta: &str) {
            if delta.is_empty() {
                return;
            }
            self.accumulated.push_str(delta);
            self.sink.on_assistant_delta(delta, self.accumulated).await;
        }

        async fn on_thinking_start(&mut self) {
            self.sink.on_thinking_start().await;
        }

        async fn on_thinking_delta(&mut self, delta: &str) {
            if delta.is_empty() {
                return;
            }
            self.sink.on_thinking_delta(delta).await;
        }

        async fn on_thinking_end(&mut self) {
            self.sink.on_thinking_end().await;
        }
    }

    for _turn in 0..max {
        total_turns += 1;

        // Call the LLM
        let response: ChatCompletionResponse = if let Some(agent_sink) = sink.as_deref_mut() {
            let mut provider_sink = ExecutionProviderSink {
                sink: agent_sink,
                accumulated: &mut accumulated_assistant,
            };
            provider
                .chat_completion_stream(
                    Some(system_prompt),
                    &messages,
                    &tool_defs,
                    model,
                    temperature,
                    DEFAULT_MAX_TOKENS,
                    Some(&mut provider_sink),
                )
                .await?
        } else {
            provider
                .chat_completion_stream(
                    Some(system_prompt),
                    &messages,
                    &tool_defs,
                    model,
                    temperature,
                    DEFAULT_MAX_TOKENS,
                    None,
                )
                .await?
        };

        // If no tool_use in response, we're done — return the text
        if !response.has_tool_use() {
            let output = response.text();
            // If provider streaming didn't emit deltas, fall back to one-shot.
            if !output.is_empty() && accumulated_assistant.is_empty() {
                accumulated_assistant.push_str(&output);
                if let Some(s) = sink.as_deref_mut() {
                    s.on_assistant_delta(&output, &accumulated_assistant).await;
                }
            }
            return Ok(AgentExecutionResult {
                output,
                success: true,
                turns: total_turns,
                tool_calls: total_tool_calls,
                duration_ms: start.elapsed().as_millis() as u64,
                error: None,
                tool_traces,
            });
        }

        // Response contains tool calls — process them

        // First, add the assistant message with tool_use blocks to conversation
        messages.push(ChatMessage {
            role: "assistant".into(),
            content: ChatContent::Blocks(response.content.clone()),
        });

        // Execute each tool call and collect results
        let mut result_blocks: Vec<ContentBlock> = Vec::new();

        for (call_id, tool_name, tool_input) in response.tool_uses() {
            total_tool_calls += 1;
            let tool_started = Instant::now();
            let tool_input_owned = tool_input.clone();
            if let Some(s) = sink.as_deref_mut() {
                s.on_tool_start(call_id, tool_name, &tool_input_owned).await;
            }

            // Find the matching tool
            let tool = tools.iter().find(|t| t.name() == tool_name);

            let mut external_execution: Option<(String, bool)> = None;
            if let Some(ctx) = external_tool_context.as_ref() {
                match ctx
                    .executor
                    .execute_external_tool(&ExternalToolCall {
                        tenant_id: ctx.tenant_id.clone(),
                        chat_id: ctx.chat_id.clone(),
                        run_id: ctx.run_id.clone(),
                        call_id: call_id.to_string(),
                        tool_name: tool_name.to_string(),
                        tool_input: tool_input.clone(),
                    })
                    .await
                {
                    Ok(Some(result)) => {
                        external_execution = Some((result.output, result.is_error));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        external_execution =
                            Some((format!("External tool execution error: {e}"), true));
                    }
                }
            }

            let external_execution_handled = external_execution.is_some();
            let (result_content, is_error) = if let Some(external_result) = external_execution {
                external_result
            } else if execution_environment.block_local_fallback(tool_name, external_execution_handled)
            {
                (
                    format!(
                        "Tool '{tool_name}' requires local-bridge execution in production mode; backend fallback is disabled"
                    ),
                    true,
                )
            } else {
                match tool {
                    Some(tool) => match tool.execute(tool_input.clone()).await {
                        Ok(result) => {
                            if result.success {
                                (result.output, false)
                            } else {
                                (
                                    format!(
                                        "Error: {}",
                                        result.error.unwrap_or_else(|| "Unknown error".into())
                                    ),
                                    true,
                                )
                            }
                        }
                        Err(e) => (format!("Tool execution error: {e}"), true),
                    },
                    None => (format!("Unknown tool: {tool_name}"), true),
                }
            };
            let tool_duration_ms = tool_started.elapsed().as_millis() as u64;

            if let Some(s) = sink.as_deref_mut() {
                s.on_tool_end(
                    call_id,
                    tool_name,
                    &result_content,
                    is_error,
                    tool_duration_ms,
                )
                .await;
            }

            tool_traces.push(AgentToolTrace {
                id: call_id.to_string(),
                name: tool_name.to_string(),
                args: tool_input_owned,
                result: result_content.clone(),
                is_error,
                duration_ms: tool_duration_ms,
            });

            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: call_id.to_string(),
                content: result_content,
                is_error,
            });
        }

        // Add tool results as a user message (Anthropic convention)
        messages.push(ChatMessage {
            role: "user".into(),
            content: ChatContent::Blocks(result_blocks),
        });
    }

    // Max turns exhausted — return whatever text we have
    let last_text = messages
        .iter()
        .rev()
        .find_map(|m| match &m.content {
            ChatContent::Text(t) => Some(t.clone()),
            ChatContent::Blocks(blocks) => {
                let text: String = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
        })
        .unwrap_or_default();

    Ok(AgentExecutionResult {
        output: last_text,
        success: true,
        turns: total_turns,
        tool_calls: total_tool_calls,
        duration_ms: start.elapsed().as_millis() as u64,
        error: Some(format!("Agent reached max turns ({max})")),
        tool_traces,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_execution_environment_from_env_defaults_to_dev() {
        unsafe {
            std::env::remove_var("ARIA_ENVIRONMENT");
            std::env::remove_var("AFW_ENV");
        }
        assert_eq!(ToolExecutionEnvironment::from_env(), ToolExecutionEnvironment::Dev);
    }

    #[test]
    fn tool_execution_environment_from_env_reads_prod() {
        unsafe {
            std::env::set_var("ARIA_ENVIRONMENT", "production");
            std::env::remove_var("AFW_ENV");
        }
        assert_eq!(ToolExecutionEnvironment::from_env(), ToolExecutionEnvironment::Prod);
        unsafe {
            std::env::remove_var("ARIA_ENVIRONMENT");
        }
    }

    #[test]
    fn prod_blocks_local_fallback_for_local_tools() {
        assert!(ToolExecutionEnvironment::Prod.block_local_fallback("shell", false));
        assert!(ToolExecutionEnvironment::Prod.block_local_fallback("file_read", false));
        assert!(ToolExecutionEnvironment::Prod.block_local_fallback("file_write", false));
        assert!(!ToolExecutionEnvironment::Prod.block_local_fallback("browser_open", false));
        assert!(!ToolExecutionEnvironment::Prod.block_local_fallback("shell", true));
    }

    #[test]
    fn tool_to_definition_works() {
        use crate::security::SecurityPolicy;
        use crate::tools::ShellTool;
        use std::sync::Arc;

        let security = Arc::new(SecurityPolicy::default());
        let tool = ShellTool::new(security);
        let def = tool_to_definition(&tool);
        assert_eq!(def.name, "shell");
        assert!(!def.description.is_empty());
        assert!(def.input_schema.is_object());
    }

    #[test]
    fn agent_execution_result_defaults() {
        let result = AgentExecutionResult {
            output: "hello".into(),
            success: true,
            turns: 1,
            tool_calls: 0,
            duration_ms: 42,
            error: None,
            tool_traces: Vec::new(),
        };
        assert!(result.success);
        assert_eq!(result.output, "hello");
    }
}
