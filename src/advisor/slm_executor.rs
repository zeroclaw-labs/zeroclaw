//! SLM Executor — Gemma 4 loops through tool calls via a prompt-guided
//! XML protocol.
//!
//! # Why prompt-guided, not native tool-calling?
//!
//! Ollama's `tools:` parameter only exposes native function-calling for
//! a small subset of its supported models. Gemma 4 (the on-device SLM
//! we target in MoA) does not currently respect that parameter — it
//! emits tool calls as structured text in the assistant turn. To stay
//! portable across future Ollama models too, we drive tool invocation
//! with an XML tag protocol embedded in the system prompt:
//!
//! ```text
//! <tool_call>
//! {"name": "smart_search", "arguments": {"query": "..."}}
//! </tool_call>
//! ```
//!
//! The executor loop reads each SLM reply, extracts the first
//! `<tool_call>` block (if any), dispatches the named tool, feeds the
//! result back as a synthetic `<tool_result>` user turn, and lets the
//! SLM continue. When a reply contains **no** `<tool_call>` we treat
//! it as the final answer and return.
//!
//! # Fail-safe fallback
//!
//! Real SLMs are imperfect at strict JSON. To keep the executor robust
//! we:
//!   * Accept stray prose around the XML tags.
//!   * Surface tool execution errors back to the SLM as
//!     `<tool_error>…</tool_error>` so it can course-correct instead
//!     of hanging.
//!   * Hard-cap the iteration count; on overrun we return the last
//!     assistant message we saw rather than panicking.
//!   * Expose a `RunOutcome` that marks `exceeded_iterations: bool`
//!     so the caller (handle_api_chat) can fall back to the full LLM
//!     agent loop when the SLM can't close the task alone.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::providers::traits::ChatMessage;
use crate::providers::Provider;
use crate::tools::traits::Tool;

/// One iteration's outcome — used for tests and tracing. Not part of
/// the public return type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum StepKind {
    /// SLM emitted a final answer (no tool call).
    Final,
    /// SLM asked for a tool; we dispatched it.
    ToolDispatched { tool: String },
    /// SLM emitted a tool_call tag but the JSON / tool name was invalid.
    /// We surfaced the error back to the SLM so it can try again.
    ToolParseError,
    /// Tool executed but returned `success=false`. Error surfaced back.
    ToolExecError { tool: String },
}

/// Final outcome of an executor run, plus a trace of what happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutcome {
    /// The SLM's last textual reply — either the final answer or the
    /// best we got before hitting `max_iterations`.
    pub reply: String,
    /// Whether the loop terminated because we exceeded `max_iterations`.
    /// When `true`, callers should treat the reply as low-confidence and
    /// usually fall back to the full LLM agent loop.
    pub exceeded_iterations: bool,
    /// Names of tools invoked, in invocation order. Lets the advisor
    /// REVIEW checkpoint and the response body show "which tools ran".
    pub tools_invoked: Vec<String>,
    /// Iteration count actually used (1..=max_iterations).
    pub iterations: usize,
}

/// On-device SLM driven by a prompt-guided tool-calling loop.
pub struct SlmExecutor {
    provider: Arc<dyn Provider>,
    model: String,
    temperature: f64,
    max_iterations: usize,
    /// Per-call timeout for each SLM invocation. Independent of the
    /// overall loop — the loop can take `max_iterations * timeout`
    /// wall-clock time in the worst case.
    per_step_timeout: Duration,
}

impl SlmExecutor {
    /// Construct an executor. `provider` must be the Ollama provider
    /// instance pointed at Gemma 4 (or whichever on-device model is
    /// configured); `tools` is the shared registry the executor will
    /// dispatch to.
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
        temperature: f64,
        max_iterations: usize,
    ) -> Self {
        Self {
            provider,
            model: model.into(),
            temperature,
            max_iterations: max_iterations.max(1),
            per_step_timeout: Duration::from_secs(45),
        }
    }

    /// Override the per-step SLM call timeout. Default 45s.
    #[must_use]
    pub fn with_step_timeout(mut self, timeout: Duration) -> Self {
        self.per_step_timeout = timeout;
        self
    }

    /// Model id in use.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Run the executor loop for a single user task.
    ///
    /// `task` is the enriched user message (including advisor PLAN and
    /// any history context). `tools` is a slice of borrows over the
    /// caller's tool registry — `&[&dyn Tool]` lets both
    /// `Arc<Vec<Box<dyn Tool>>>` and `Vec<Arc<dyn Tool>>` feed in
    /// without reallocating.
    pub async fn run(&self, task: &str, tools: &[&dyn Tool]) -> Result<RunOutcome> {
        let mut conversation: Vec<ChatMessage> = Vec::with_capacity(self.max_iterations * 2 + 2);
        conversation.push(ChatMessage::system(build_system_prompt(tools)));
        conversation.push(ChatMessage::user(task));

        let mut tools_invoked: Vec<String> = Vec::new();
        let mut last_reply = String::new();

        for iteration in 1..=self.max_iterations {
            let reply_fut = self.provider.chat_with_history(
                &conversation,
                &self.model,
                self.temperature,
            );
            let reply = match tokio::time::timeout(self.per_step_timeout, reply_fut).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    warn!(iteration, error = %e, "SLM executor call failed");
                    return Err(e);
                }
                Err(_) => {
                    warn!(iteration, timeout = ?self.per_step_timeout, "SLM executor step timed out");
                    return Err(anyhow!("SLM executor step timed out"));
                }
            };
            last_reply = reply.clone();
            conversation.push(ChatMessage::assistant(&reply));

            match parse_tool_call(&reply) {
                None => {
                    debug!(iteration, "SLM executor produced final answer");
                    return Ok(RunOutcome {
                        reply: strip_control_tags(&reply),
                        exceeded_iterations: false,
                        tools_invoked,
                        iterations: iteration,
                    });
                }
                Some(Err(parse_err)) => {
                    // Malformed tool call — feed the error back so the
                    // SLM can retry. Counts against the iteration budget.
                    debug!(iteration, error = %parse_err, "SLM emitted malformed tool_call");
                    conversation.push(ChatMessage::user(format!(
                        "<tool_error>\nFailed to parse tool_call: {parse_err}. Respond again with a valid <tool_call>…</tool_call> block or a final answer.\n</tool_error>"
                    )));
                }
                Some(Ok(call)) => {
                    info!(
                        iteration,
                        tool = call.name.as_str(),
                        "SLM executor dispatching tool"
                    );
                    match find_tool(tools, &call.name) {
                        Some(tool) => {
                            let exec_result = tool.execute(call.arguments).await;
                            match exec_result {
                                Ok(tr) if tr.success => {
                                    tools_invoked.push(call.name.clone());
                                    conversation.push(ChatMessage::user(format!(
                                        "<tool_result tool=\"{}\">\n{}\n</tool_result>",
                                        call.name, tr.output
                                    )));
                                }
                                Ok(tr) => {
                                    let err = tr.error.unwrap_or_else(|| {
                                        "tool reported success=false with no error message".into()
                                    });
                                    conversation.push(ChatMessage::user(format!(
                                        "<tool_error tool=\"{}\">\n{err}\n</tool_error>",
                                        call.name
                                    )));
                                }
                                Err(e) => {
                                    conversation.push(ChatMessage::user(format!(
                                        "<tool_error tool=\"{}\">\nexecution threw: {e}\n</tool_error>",
                                        call.name
                                    )));
                                }
                            }
                        }
                        None => {
                            conversation.push(ChatMessage::user(format!(
                                "<tool_error>\nUnknown tool '{name}'. Available tools: {avail}. Try again with one of those or emit a final answer.\n</tool_error>",
                                name = call.name,
                                avail = tools.iter().map(|t| t.name()).collect::<Vec<_>>().join(", "),
                            )));
                        }
                    }
                }
            }
        }

        warn!(
            iterations = self.max_iterations,
            "SLM executor hit max_iterations — returning last reply"
        );
        Ok(RunOutcome {
            reply: strip_control_tags(&last_reply),
            exceeded_iterations: true,
            tools_invoked,
            iterations: self.max_iterations,
        })
    }
}

/// A parsed tool call from the SLM's assistant turn.
#[derive(Debug, Clone, PartialEq)]
struct ParsedToolCall {
    name: String,
    arguments: serde_json::Value,
}

/// Parse a reply for a `<tool_call>…</tool_call>` block.
///
/// Returns:
/// - `None` if no tool call tag is present — caller treats reply as final answer.
/// - `Some(Ok(..))` if the tag is present and the JSON parsed correctly.
/// - `Some(Err(..))` if a tag is present but the body is malformed — the
///   loop surfaces the error back to the SLM for a retry.
fn parse_tool_call(reply: &str) -> Option<Result<ParsedToolCall>> {
    let open = reply.find("<tool_call>")?;
    let rest = &reply[open + "<tool_call>".len()..];
    let close = rest.find("</tool_call>");
    let body = match close {
        Some(c) => &rest[..c],
        None => rest, // unclosed tag — salvage what we can
    };
    let trimmed = body.trim();
    // Strip optional ```json fences
    let cleaned = if let Some(s) = trimmed.strip_prefix("```json") {
        s.trim().trim_end_matches("```").trim()
    } else if let Some(s) = trimmed.strip_prefix("```") {
        s.trim().trim_end_matches("```").trim()
    } else {
        trimmed
    };
    match serde_json::from_str::<serde_json::Value>(cleaned) {
        Ok(value) => {
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            match name {
                Some(name) if !name.is_empty() => Some(Ok(ParsedToolCall {
                    name,
                    arguments: value
                        .get("arguments")
                        .cloned()
                        .or_else(|| value.get("args").cloned())
                        .unwrap_or_else(|| serde_json::json!({})),
                })),
                _ => Some(Err(anyhow!("tool_call missing `name` field"))),
            }
        }
        Err(e) => Some(Err(anyhow!("tool_call JSON invalid: {e}"))),
    }
}

fn find_tool<'a>(tools: &'a [&'a dyn Tool], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().find(|t| t.name() == name).copied()
}

/// Remove any stray `<tool_call>` / `<tool_result>` / `<tool_error>`
/// blocks a user will never want to read. Used when handing the final
/// reply back to the response body.
fn strip_control_tags(reply: &str) -> String {
    let mut out = reply.to_string();
    for tag in ["tool_call", "tool_result", "tool_error"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        while let Some(start) = out.find(&open) {
            // `<tool_result tool="X">` has extra attrs — find the first `>`.
            let gt = match out[start..].find('>') {
                Some(g) => start + g + 1,
                None => break,
            };
            if let Some(end) = out[gt..].find(&close) {
                out.drain(start..(gt + end + close.len()));
            } else {
                // unclosed tag — drop from `start` onwards.
                out.truncate(start);
                break;
            }
        }
    }
    out.trim().to_string()
}

/// Build the system prompt that defines the executor's role and the
/// tool-call protocol. Called once per `run()` because tool specs are
/// embedded inline (SLMs don't benefit from cache hits the way
/// cloud LLMs do).
fn build_system_prompt(tools: &[&dyn Tool]) -> String {
    let mut out = String::new();
    out.push_str(
        "You are the on-device executor in a two-model team. A strategic \
         advisor has given you a plan; your job is to carry it out, using \
         tools when you need real-world information or actions, and \
         producing a final answer for the user.\n\n\
         ## Protocol\n\n\
         When you want to call a tool, emit EXACTLY one block like this \
         and stop speaking:\n\n\
         <tool_call>\n\
         {\"name\": \"tool_name\", \"arguments\": { ... }}\n\
         </tool_call>\n\n\
         You will receive the tool's output in the next turn as \
         <tool_result tool=\"tool_name\">…</tool_result>, or \
         <tool_error tool=\"…\">…</tool_error> if it failed. Read the \
         result, then EITHER call another tool OR produce the final \
         answer as plain prose.\n\n\
         Do not emit a tool_call if you already have enough information \
         to answer the user. Prefer `smart_search` over `web_search` \
         and `perplexity_search` for web lookups — it cascades the \
         free tier to paid tier to retries automatically.\n\n\
         ## Available tools\n\n",
    );
    for tool in tools {
        let _ = std::fmt::Write::write_fmt(
            &mut out,
            format_args!(
                "- `{}` — {}\n  parameters: {}\n",
                tool.name(),
                tool.description().lines().next().unwrap_or(""),
                tool.parameters_schema(),
            ),
        );
    }
    out.push_str(
        "\n## Output shape\n\n\
         - Call tools until you have enough to answer. When ready, write \
         the final answer as plain text. Do NOT wrap the final answer in \
         any XML tag. Do NOT include a tool_call and an answer in the \
         same turn.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_call_handles_simple_json() {
        let reply = r#"sure. <tool_call>{"name":"smart_search","arguments":{"query":"rust"}}</tool_call>"#;
        let parsed = parse_tool_call(reply).expect("has tool call").unwrap();
        assert_eq!(parsed.name, "smart_search");
        assert_eq!(parsed.arguments["query"], "rust");
    }

    #[test]
    fn parse_tool_call_accepts_fenced_json() {
        let reply = "<tool_call>\n```json\n{\"name\":\"x\",\"arguments\":{}}\n```\n</tool_call>";
        let parsed = parse_tool_call(reply).expect("has tool call").unwrap();
        assert_eq!(parsed.name, "x");
    }

    #[test]
    fn parse_tool_call_returns_none_without_tag() {
        assert!(parse_tool_call("just a final answer, no tool needed").is_none());
    }

    #[test]
    fn parse_tool_call_reports_invalid_json() {
        let reply = "<tool_call>{not_json_at_all}</tool_call>";
        let result = parse_tool_call(reply).expect("has tool call");
        assert!(result.is_err());
    }

    #[test]
    fn parse_tool_call_flags_missing_name() {
        let reply = "<tool_call>{\"arguments\":{}}</tool_call>";
        let result = parse_tool_call(reply).expect("has tool call");
        assert!(result.is_err());
    }

    #[test]
    fn parse_tool_call_accepts_args_alias() {
        let reply = r#"<tool_call>{"name":"f","args":{"k":"v"}}</tool_call>"#;
        let parsed = parse_tool_call(reply).expect("has tool call").unwrap();
        assert_eq!(parsed.arguments["k"], "v");
    }

    #[test]
    fn parse_tool_call_salvages_unclosed_tag() {
        let reply = r#"<tool_call>{"name":"ok","arguments":{}}"#;
        let parsed = parse_tool_call(reply).expect("has tool call").unwrap();
        assert_eq!(parsed.name, "ok");
    }

    #[test]
    fn strip_control_tags_removes_blocks() {
        let msg = "Hello <tool_call>{...}</tool_call> world <tool_result tool=\"x\">stuff</tool_result> end";
        let cleaned = strip_control_tags(msg);
        assert_eq!(cleaned, "Hello  world  end");
    }

    #[test]
    fn strip_control_tags_handles_unclosed_tag() {
        let msg = "intro <tool_call>{partial";
        let cleaned = strip_control_tags(msg);
        assert_eq!(cleaned, "intro");
    }

    #[test]
    fn build_system_prompt_mentions_protocol_and_tools() {
        use async_trait::async_trait;

        struct Stub;
        #[async_trait]
        impl Tool for Stub {
            fn name(&self) -> &str {
                "stub_tool"
            }
            fn description(&self) -> &str {
                "a stub tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(
                &self,
                _: serde_json::Value,
            ) -> anyhow::Result<crate::tools::traits::ToolResult> {
                unreachable!()
            }
        }
        let stub = Stub;
        let tools: Vec<&dyn Tool> = vec![&stub];
        let prompt = build_system_prompt(&tools);
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("stub_tool"));
        assert!(prompt.contains("a stub tool"));
    }
}
