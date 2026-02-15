//! Reusable agentic executor — runs a multi-turn tool-use loop against an LLM.
//!
//! This is the core "brain" of Aria. Given a provider, tools, system prompt,
//! and user input, it runs the agent loop:
//!   1. Send messages + tool specs to the LLM
//!   2. If response contains `tool_use` blocks → execute tools → append results
//!   3. Loop until the LLM emits a final text response (no tool calls)
//!   4. Return the final text and execution metadata

use crate::providers::{
    ChatCompletionResponse, ChatContent, ChatMessage, ContentBlock, Provider, ToolDefinition,
};
use crate::tools::Tool;
use anyhow::Result;
use std::time::Instant;

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
    let start = Instant::now();
    let max = max_turns.unwrap_or(DEFAULT_MAX_TURNS);

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

    for _turn in 0..max {
        total_turns += 1;

        // Call the LLM
        let response: ChatCompletionResponse = provider
            .chat_completion(
                Some(system_prompt),
                &messages,
                &tool_defs,
                model,
                temperature,
                DEFAULT_MAX_TOKENS,
            )
            .await?;

        // If no tool_use in response, we're done — return the text
        if !response.has_tool_use() {
            let output = response.text();
            return Ok(AgentExecutionResult {
                output,
                success: true,
                turns: total_turns,
                tool_calls: total_tool_calls,
                duration_ms: start.elapsed().as_millis() as u64,
                error: None,
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

            // Find the matching tool
            let tool = tools.iter().find(|t| t.name() == tool_name);

            let result_content = match tool {
                Some(tool) => match tool.execute(tool_input.clone()).await {
                    Ok(result) => {
                        if result.success {
                            result.output
                        } else {
                            format!(
                                "Error: {}",
                                result.error.unwrap_or_else(|| "Unknown error".into())
                            )
                        }
                    }
                    Err(e) => format!("Tool execution error: {e}"),
                },
                None => format!("Unknown tool: {tool_name}"),
            };

            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: call_id.to_string(),
                content: result_content,
                is_error: false,
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        assert!(result.success);
        assert_eq!(result.output, "hello");
    }
}
