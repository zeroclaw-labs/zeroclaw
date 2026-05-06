//! Tool execution helpers extracted from `loop_`.
//!
//! Contains the functions responsible for invoking tools (single, parallel,
//! sequential) and the decision logic for choosing between parallel and
//! sequential execution.

use anyhow::Result;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::approval::ApprovalManager;
use crate::observability::{Observer, ObserverEvent};
use crate::tools::Tool;

// Items that still live in `loop_` — import via the parent module.
use super::loop_::{ParsedToolCall, ToolLoopCancelled, scrub_credentials};

// ── Helpers ──────────────────────────────────────────────────────────────

/// Look up a tool by name in a slice of boxed `dyn Tool` values.
pub fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
}

// ── Outcome ──────────────────────────────────────────────────────────────

pub struct ToolExecutionOutcome {
    pub output: String,
    pub success: bool,
    pub error_reason: Option<String>,
    pub duration: Duration,
    /// Cryptographic HMAC receipt proving this tool actually executed.
    /// Present only when tool receipts are enabled in config.
    pub receipt: Option<String>,
}

// ── Single tool execution ────────────────────────────────────────────────

pub async fn execute_one_tool(
    call_name: &str,
    call_arguments: serde_json::Value,
    tool_call_id: Option<&str>,
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
) -> Result<ToolExecutionOutcome> {
    // Serialize arguments once and carry the full JSON into both observer
    // events. Previously the start event received a 300-char summary and the
    // completion event received no arguments at all, which made tool spans
    // opaque in OTel backends (see upstream issue #5980 — "Otel Traces Should
    // Include More Details About Why A Tool Call Failed"). Size is bounded
    // downstream by the tracing exporter, so we don't need to clip here.
    let full_args = call_arguments.to_string();
    let tool_call_id_owned = tool_call_id.map(str::to_string);
    observer.record_event(&ObserverEvent::ToolCallStart {
        tool: call_name.to_string(),
        tool_call_id: tool_call_id_owned.clone(),
        arguments: Some(full_args.clone()),
    });
    let start = Instant::now();

    let static_tool = find_tool(tools_registry, call_name);
    let activated_arc = if static_tool.is_none() {
        activated_tools.and_then(|at| at.lock().unwrap().get_resolved(call_name))
    } else {
        None
    };
    let Some(tool) = static_tool.or(activated_arc.as_deref()) else {
        let reason = format!("Unknown tool: {call_name}");
        let duration = start.elapsed();
        let scrubbed_reason = scrub_credentials(&reason);
        observer.record_event(&ObserverEvent::ToolCall {
            tool: call_name.to_string(),
            tool_call_id: tool_call_id_owned.clone(),
            duration,
            success: false,
            arguments: Some(full_args.clone()),
            result: Some(scrubbed_reason.clone()),
        });
        return Ok(ToolExecutionOutcome {
            output: reason,
            success: false,
            error_reason: Some(scrubbed_reason),
            duration,
            receipt: None,
        });
    };

    let tool_future = tool.execute(call_arguments.clone());
    let tool_result = if let Some(token) = cancellation_token {
        tokio::select! {
            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
            result = tool_future => result,
        }
    } else {
        tool_future.await
    };

    match tool_result {
        Ok(r) => {
            let duration = start.elapsed();
            if r.success {
                let normalized_output = if r.output.is_empty() {
                    "(no output)"
                } else {
                    &r.output
                };
                let output = scrub_credentials(normalized_output);
                let receipt = receipt_generator.map(|receipt_gen| {
                    receipt_gen.generate_now(call_name, &call_arguments, &output)
                });
                observer.record_event(&ObserverEvent::ToolCall {
                    tool: call_name.to_string(),
                    tool_call_id: tool_call_id_owned.clone(),
                    duration,
                    success: true,
                    arguments: Some(full_args.clone()),
                    result: Some(output.clone()),
                });
                Ok(ToolExecutionOutcome {
                    output,
                    success: true,
                    error_reason: None,
                    duration,
                    receipt,
                })
            } else {
                let reason = r.error.unwrap_or(r.output);
                let scrubbed_reason = scrub_credentials(&reason);
                observer.record_event(&ObserverEvent::ToolCall {
                    tool: call_name.to_string(),
                    tool_call_id: tool_call_id_owned.clone(),
                    duration,
                    success: false,
                    arguments: Some(full_args.clone()),
                    result: Some(scrubbed_reason.clone()),
                });
                Ok(ToolExecutionOutcome {
                    output: format!("Error: {reason}"),
                    success: false,
                    error_reason: Some(scrubbed_reason),
                    duration,
                    receipt: None,
                })
            }
        }
        Err(e) => {
            let duration = start.elapsed();
            let reason = format!("Error executing {call_name}: {e}");
            let scrubbed_reason = scrub_credentials(&reason);
            observer.record_event(&ObserverEvent::ToolCall {
                tool: call_name.to_string(),
                tool_call_id: tool_call_id_owned.clone(),
                duration,
                success: false,
                arguments: Some(full_args.clone()),
                result: Some(scrubbed_reason.clone()),
            });
            Ok(ToolExecutionOutcome {
                output: reason,
                success: false,
                error_reason: Some(scrubbed_reason),
                duration,
                receipt: None,
            })
        }
    }
}

// ── Parallel / sequential decision ───────────────────────────────────────

pub fn should_execute_tools_in_parallel(
    tool_calls: &[ParsedToolCall],
    approval: Option<&ApprovalManager>,
) -> bool {
    if tool_calls.len() <= 1 {
        return false;
    }

    // tool_search activates deferred MCP tools into ActivatedToolSet.
    // Running tool_search in parallel with the tools it activates causes a
    // race condition where the tool lookup happens before activation completes.
    // Force sequential execution whenever tool_search is in the batch.
    if tool_calls.iter().any(|call| call.name == "tool_search") {
        return false;
    }

    if let Some(mgr) = approval
        && tool_calls.iter().any(|call| mgr.needs_approval(&call.name))
    {
        // Approval-gated calls must keep sequential handling so the caller can
        // enforce CLI prompt/deny policy consistently.
        return false;
    }

    true
}

// ── Parallel execution ───────────────────────────────────────────────────

pub async fn execute_tools_parallel(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
) -> Result<Vec<ToolExecutionOutcome>> {
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|call| {
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                call.tool_call_id.as_deref(),
                tools_registry,
                activated_tools,
                observer,
                cancellation_token,
                receipt_generator,
            )
        })
        .collect();

    let results = futures_util::future::join_all(futures).await;
    results.into_iter().collect()
}

// ── Sequential execution ─────────────────────────────────────────────────

pub async fn execute_tools_sequential(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
) -> Result<Vec<ToolExecutionOutcome>> {
    let mut outcomes = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        outcomes.push(
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                call.tool_call_id.as_deref(),
                tools_registry,
                activated_tools,
                observer,
                cancellation_token,
                receipt_generator,
            )
            .await?,
        );
    }

    Ok(outcomes)
}
