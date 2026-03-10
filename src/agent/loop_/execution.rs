use super::parsing::ParsedToolCall;
use super::{scrub_credentials, ToolLoopCancelled};
use crate::approval::ApprovalManager;
use crate::observability::{session_recorder::SessionRecorder, Observer, ObserverEvent};
use crate::tools::Tool;
use anyhow::Result;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
}
async fn execute_one_tool(
    call_name: &str,
    call_arguments: serde_json::Value,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    session_recorder: Option<&SessionRecorder>,
) -> Result<ToolExecutionOutcome> {
    observer.record_event(&ObserverEvent::ToolCallStart {
        tool: call_name.to_string(),
    });
    let args_preview = {
        let s = call_arguments.to_string();
        if s.chars().count() > 400 {
            let truncated: String = s.chars().take(400).collect();
            format!("{}…", truncated)
        } else {
            s
        }
    };
    tracing::info!(tool = %call_name, args = %args_preview, "tool.invoke");
    let start = Instant::now();

    let Some(tool) = find_tool(tools_registry, call_name) else {
        let reason = format!("Unknown tool: {call_name}");
        let duration = start.elapsed();
        observer.record_event(&ObserverEvent::ToolCall {
            tool: call_name.to_string(),
            duration,
            success: false,
        });
        return Ok(ToolExecutionOutcome {
            output: reason.clone(),
            success: false,
            error_reason: Some(scrub_credentials(&reason)),
            duration,
        });
    };

    let args_for_record = if session_recorder.is_some() {
        Some(call_arguments.clone())
    } else {
        None
    };
    let tool_future = tool.execute(call_arguments);
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
            observer.record_event(&ObserverEvent::ToolCall {
                tool: call_name.to_string(),
                duration,
                success: r.success,
            });
            let out_preview: String = scrub_credentials(&r.output).chars().take(400).collect();
            tracing::info!(tool = %call_name, ok = r.success, ms = %duration.as_millis(), out = %out_preview, "tool.done");
            if r.success {
                let scrubbed_output = scrub_credentials(&r.output);
                if let Some(rec) = session_recorder {
                    rec.record_tool_call(
                        call_name,
                        args_for_record.as_ref().unwrap_or(&serde_json::Value::Null),
                        &scrubbed_output,
                        true,
                        duration.as_millis() as u64,
                    );
                }
                Ok(ToolExecutionOutcome {
                    output: scrubbed_output,
                    success: true,
                    error_reason: None,
                    duration,
                })
            } else {
                let reason = r.error.unwrap_or(r.output);
                let scrubbed_reason = scrub_credentials(&reason);
                if let Some(rec) = session_recorder {
                    rec.record_tool_call(
                        call_name,
                        args_for_record.as_ref().unwrap_or(&serde_json::Value::Null),
                        &scrubbed_reason,
                        false,
                        duration.as_millis() as u64,
                    );
                }
                Ok(ToolExecutionOutcome {
                    output: format!("Error: {reason}"),
                    success: false,
                    error_reason: Some(scrubbed_reason),
                    duration,
                })
            }
        }
        Err(e) => {
            let duration = start.elapsed();
            observer.record_event(&ObserverEvent::ToolCall {
                tool: call_name.to_string(),
                duration,
                success: false,
            });
            tracing::info!(tool = %call_name, ok = false, ms = %duration.as_millis(), "tool.err");
            let reason = format!("Error executing {call_name}: {e}");
            let scrubbed_reason = scrub_credentials(&reason);
            if let Some(rec) = session_recorder {
                rec.record_tool_call(
                    call_name,
                    args_for_record.as_ref().unwrap_or(&serde_json::Value::Null),
                    &scrubbed_reason,
                    false,
                    duration.as_millis() as u64,
                );
            }
            Ok(ToolExecutionOutcome {
                output: reason,
                success: false,
                error_reason: Some(scrubbed_reason),
                duration,
            })
        }
    }
}

pub(super) struct ToolExecutionOutcome {
    pub(super) output: String,
    pub(super) success: bool,
    pub(super) error_reason: Option<String>,
    pub(super) duration: Duration,
}

pub(super) fn should_execute_tools_in_parallel(
    tool_calls: &[ParsedToolCall],
    approval: Option<&ApprovalManager>,
) -> bool {
    if tool_calls.len() <= 1 {
        return false;
    }

    if let Some(mgr) = approval {
        if tool_calls
            .iter()
            .any(|call| mgr.needs_approval_for_call(&call.name, &call.arguments))
        {
            // Approval-gated calls must keep sequential handling so the caller can
            // enforce CLI prompt/deny policy consistently.
            return false;
        }
    }

    true
}

pub(super) async fn execute_tools_parallel(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    session_recorder: Option<&SessionRecorder>,
) -> Result<Vec<ToolExecutionOutcome>> {
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|call| {
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                tools_registry,
                observer,
                cancellation_token,
                session_recorder,
            )
        })
        .collect();

    let results = futures_util::future::join_all(futures).await;
    results.into_iter().collect()
}

pub(super) async fn execute_tools_sequential(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    session_recorder: Option<&SessionRecorder>,
) -> Result<Vec<ToolExecutionOutcome>> {
    let mut outcomes = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        outcomes.push(
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                tools_registry,
                observer,
                cancellation_token,
                session_recorder,
            )
            .await?,
        );
    }

    Ok(outcomes)
}
