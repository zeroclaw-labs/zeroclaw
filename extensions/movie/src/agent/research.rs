//! Research phase — proactive information gathering before main response.
//!
//! When enabled, the agent runs a focused "research turn" using available tools
//! to gather context before generating its main response. This creates a
//! "thinking" phase where the agent explores the codebase, searches memory,
//! or fetches external data.
//!
//! Supports both:
//! - Native tool calling (OpenAI, Anthropic, Bedrock, etc.)
//! - Prompt-guided tool calling (Gemini and other providers without native support)

use crate::agent::dispatcher::{ToolDispatcher, XmlToolDispatcher};
use crate::config::{ResearchPhaseConfig, ResearchTrigger};
use crate::observability::Observer;
use crate::providers::traits::build_tool_instructions_text;
use crate::providers::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolCall};
use crate::tools::{Tool, ToolResult, ToolSpec};
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Result of the research phase.
#[derive(Debug, Clone)]
pub struct ResearchResult {
    /// Collected context from research (formatted for injection into main prompt).
    pub context: String,
    /// Number of tool calls made during research.
    pub tool_call_count: usize,
    /// Duration of the research phase.
    pub duration: Duration,
    /// Summary of tools called and their results.
    pub tool_summaries: Vec<ToolSummary>,
}

/// Summary of a single tool call during research.
#[derive(Debug, Clone)]
pub struct ToolSummary {
    pub tool_name: String,
    pub arguments_preview: String,
    pub result_preview: String,
    pub success: bool,
}

/// Check if research phase should be triggered for this message.
pub fn should_trigger(config: &ResearchPhaseConfig, message: &str) -> bool {
    if !config.enabled {
        return false;
    }

    match config.trigger {
        ResearchTrigger::Never => false,
        ResearchTrigger::Always => true,
        ResearchTrigger::Keywords => {
            let message_lower = message.to_lowercase();
            config
                .keywords
                .iter()
                .any(|kw| message_lower.contains(&kw.to_lowercase()))
        }
        ResearchTrigger::Length => message.len() >= config.min_message_length,
        ResearchTrigger::Question => message.contains('?'),
    }
}

/// Default system prompt for research phase.
const RESEARCH_SYSTEM_PROMPT: &str = r#"You are in RESEARCH MODE. Your task is to gather information that will help answer the user's question.

RULES:
1. Use tools to search, read files, check status, or fetch data
2. Focus on gathering FACTS, not answering yet
3. Be efficient — only gather what's needed
4. After gathering enough info, respond with a summary starting with "[RESEARCH COMPLETE]"

DO NOT:
- Answer the user's question directly
- Make changes to files
- Execute destructive commands

When you have enough information, summarize what you found in this format:
[RESEARCH COMPLETE]
- Finding 1: ...
- Finding 2: ...
- Finding 3: ...
"#;

/// Run the research phase.
///
/// This executes a focused LLM + tools loop to gather information before
/// the main response. The collected context is returned for injection
/// into the main conversation.
pub async fn run_research_phase(
    config: &ResearchPhaseConfig,
    provider: &dyn Provider,
    tools: &[Box<dyn Tool>],
    user_message: &str,
    model: &str,
    temperature: f64,
    _observer: Arc<dyn Observer>,
) -> Result<ResearchResult> {
    let start = Instant::now();
    let mut tool_summaries = Vec::new();
    let mut collected_context = String::new();
    let mut iteration = 0;

    let uses_native_tools = provider.supports_native_tools();

    // Build tool specs for native OR prompt-guided tool calling
    let tool_specs: Vec<ToolSpec> = tools
        .iter()
        .map(|t| ToolSpec {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        })
        .collect();

    // Build system prompt
    // For prompt-guided providers, include tool instructions in system prompt
    let base_prompt = if config.system_prompt_prefix.is_empty() {
        RESEARCH_SYSTEM_PROMPT.to_string()
    } else {
        format!(
            "{}\n\n{}",
            config.system_prompt_prefix, RESEARCH_SYSTEM_PROMPT
        )
    };

    let system_prompt = if uses_native_tools {
        base_prompt
    } else {
        // Prompt-guided: append tool instructions
        format!(
            "{}\n\n{}",
            base_prompt,
            build_tool_instructions_text(&tool_specs)
        )
    };

    // Conversation history for research phase
    let mut messages = vec![ChatMessage::user(format!(
        "Research the following question to gather relevant information:\n\n{}",
        user_message
    ))];

    // Research loop
    while iteration < config.max_iterations {
        iteration += 1;

        // Log research iteration if showing progress
        if config.show_progress {
            tracing::info!(iteration, "Research phase iteration");
        }

        // Build messages with system prompt as first message
        let mut full_messages = vec![ChatMessage::system(&system_prompt)];
        full_messages.extend(messages.iter().cloned());

        // Call LLM
        let request = ChatRequest {
            messages: &full_messages,
            tools: if uses_native_tools {
                Some(&tool_specs)
            } else {
                None // Prompt-guided: tools are in system prompt
            },
        };

        let response: ChatResponse = provider.chat(request, model, temperature).await?;

        // Check if research is complete
        if let Some(ref text) = response.text {
            if text.contains("[RESEARCH COMPLETE]") {
                // Extract the summary
                if let Some(idx) = text.find("[RESEARCH COMPLETE]") {
                    collected_context = text[idx..].to_string();
                }
                break;
            }
        }

        // Parse tool calls: native OR from XML in response text
        let tool_calls: Vec<ToolCall> = if uses_native_tools {
            response.tool_calls.clone()
        } else {
            // Parse XML <tool_call> tags from response text using XmlToolDispatcher
            let dispatcher = XmlToolDispatcher;
            let (_, parsed) = dispatcher.parse_response(&response);
            parsed
                .into_iter()
                .enumerate()
                .map(|(i, p)| ToolCall {
                    id: p
                        .tool_call_id
                        .unwrap_or_else(|| format!("tc_{}_{}", iteration, i)),
                    name: p.name,
                    arguments: serde_json::to_string(&p.arguments).unwrap_or_default(),
                })
                .collect()
        };

        // If no tool calls, we're done
        if tool_calls.is_empty() {
            if let Some(text) = response.text {
                collected_context = text;
            }
            break;
        }

        // Execute tool calls
        for tool_call in &tool_calls {
            let tool_result = execute_tool_call(tools, tool_call).await;

            let summary = ToolSummary {
                tool_name: tool_call.name.clone(),
                arguments_preview: truncate(&tool_call.arguments, 100),
                result_preview: truncate(&tool_result.output, 200),
                success: tool_result.success,
            };

            if config.show_progress {
                tracing::info!(
                    tool = %summary.tool_name,
                    success = summary.success,
                    "Research tool call"
                );
            }

            tool_summaries.push(summary);

            // Add tool result to conversation
            messages.push(ChatMessage::assistant(format!(
                "Called tool `{}` with arguments: {}",
                tool_call.name, tool_call.arguments
            )));
            messages.push(ChatMessage::user(format!(
                "Tool result:\n{}",
                tool_result.output
            )));
        }
    }

    let duration = start.elapsed();

    Ok(ResearchResult {
        context: collected_context,
        tool_call_count: tool_summaries.len(),
        duration,
        tool_summaries,
    })
}

/// Execute a single tool call.
async fn execute_tool_call(tools: &[Box<dyn Tool>], tool_call: &ToolCall) -> ToolResult {
    // Find the tool
    let tool = tools.iter().find(|t| t.name() == tool_call.name);

    match tool {
        Some(t) => {
            // Parse arguments
            let args: serde_json::Value = serde_json::from_str(&tool_call.arguments)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            // Execute
            match t.execute(args).await {
                Ok(result) => result,
                Err(e) => ToolResult {
                    success: false,
                    output: format!("Error: {}", e),
                    error: Some(e.to_string()),
                },
            }
        }
        None => ToolResult {
            success: false,
            output: format!("Unknown tool: {}", tool_call.name),
            error: Some(format!("Unknown tool: {}", tool_call.name)),
        },
    }
}

/// Truncate string with ellipsis.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_trigger_never() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Never,
            ..Default::default()
        };
        assert!(!should_trigger(&config, "find something"));
    }

    #[test]
    fn should_trigger_always() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Always,
            ..Default::default()
        };
        assert!(should_trigger(&config, "hello"));
    }

    #[test]
    fn should_trigger_keywords() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Keywords,
            keywords: vec!["find".into(), "search".into()],
            ..Default::default()
        };
        assert!(should_trigger(&config, "please find the file"));
        assert!(should_trigger(&config, "SEARCH for errors"));
        assert!(!should_trigger(&config, "hello world"));
    }

    #[test]
    fn should_trigger_length() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Length,
            min_message_length: 20,
            ..Default::default()
        };
        assert!(!should_trigger(&config, "short"));
        assert!(should_trigger(
            &config,
            "this is a longer message that exceeds the minimum"
        ));
    }

    #[test]
    fn should_trigger_question() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Question,
            ..Default::default()
        };
        assert!(should_trigger(&config, "what is this?"));
        assert!(!should_trigger(&config, "do this now"));
    }

    #[test]
    fn disabled_never_triggers() {
        let config = ResearchPhaseConfig {
            enabled: false,
            trigger: ResearchTrigger::Always,
            ..Default::default()
        };
        assert!(!should_trigger(&config, "anything"));
    }
}
