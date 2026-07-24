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
use crate::tools::{ActivatedToolSet, Tool};
use tokio::sync::mpsc::Sender;
use zeroclaw_api::agent::TurnEvent;

// Items that still live in `loop_` — import via the parent module.
use super::loop_::{ParsedToolCall, ToolLoopCancelled, is_tool_loop_cancelled, scrub_credentials};
use super::turn::TurnMeta;

// ── Helpers ──────────────────────────────────────────────────────────────

/// If a just-completed tool call was a successful `TodoWrite`, build the
/// corresponding `TurnEvent::Plan` from its arguments. Returns `None`
/// for any other tool, a failed call, or arguments that fail to parse
/// (defensive — a real failure would already have `success == false`).
fn maybe_plan_event(
    call_name: &str,
    success: bool,
    call_arguments: &serde_json::Value,
) -> Option<zeroclaw_api::agent::TurnEvent> {
    if call_name != "TodoWrite" || !success {
        return None;
    }
    let entries = crate::tools::todo_write::parse_entries(call_arguments).ok()?;
    Some(zeroclaw_api::agent::TurnEvent::Plan { entries })
}

/// Look up a tool by name in a slice of boxed `dyn Tool` values.
pub fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
}

#[derive(Clone, Copy)]
pub(crate) struct ToolDispatchContext<'a> {
    pub tools_registry: &'a [Box<dyn Tool>],
    pub activated_tools: Option<&'a std::sync::Arc<std::sync::Mutex<ActivatedToolSet>>>,
    pub excluded_tools: &'a [String],
    /// Per-request tool allow-set (chat-completions `tools` parameter). When
    /// set, tools found in the registry but absent from this set are rejected
    /// as unavailable for this request. `None` for default (non-chat-completions)
    /// paths where the full agent tool set is available.
    pub request_tool_names: Option<&'a std::collections::HashSet<String>>,
}

fn is_excluded_tool(name: &str, excluded_tools: &[String]) -> bool {
    let name = name.trim();
    excluded_tools
        .iter()
        .any(|excluded| excluded.trim().eq_ignore_ascii_case(name))
}

fn unavailable_tool_outcome(
    call_name: &str,
    tool_call_id_owned: Option<String>,
    full_args: &str,
    meta: &TurnMeta<'_>,
    observer: &dyn Observer,
    duration: Duration,
) -> ToolExecutionOutcome {
    let reason = format!("Tool not available in this turn: {call_name}");
    observer.record_event(&ObserverEvent::ToolCall {
        tool: call_name.to_string(),
        tool_call_id: tool_call_id_owned,
        duration,
        success: false,
        arguments: Some(full_args.to_string()),
        result: Some(scrub_credentials(&reason)),
        channel: Some(meta.channel_name.to_string()),
        agent_alias: meta.agent_alias.map(|s| s.to_string()),
        parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
        turn_id: Some(meta.turn_id.to_string()),
    });
    ToolExecutionOutcome {
        output: reason.clone(),
        success: false,
        error_reason: Some(reason),
        duration,
        receipt: None,
        output_data: None,
    }
}

// ── Outcome ──────────────────────────────────────────────────────────────

pub struct ToolExecutionOutcome {
    pub output: String,
    /// Structured output when the tool declared one (`ToolOutput::data`).
    /// Feeds SOP step capture and data-flow surfaces; the LLM sees only
    /// `output`.
    pub output_data: Option<serde_json::Value>,
    pub success: bool,
    /// Raw failure text on the data path. Credential scrubbing is a rendering
    /// concern applied at each human-facing surface (observer events,
    /// post-execution log line, CLI progress), never stored pre-scrubbed here.
    pub error_reason: Option<String>,
    pub duration: Duration,
    /// Cryptographic HMAC receipt proving this tool actually executed.
    /// Present only when tool receipts are enabled in config.
    pub receipt: Option<String>,
}

// ── Single tool execution ────────────────────────────────────────────────

pub(crate) async fn execute_one_tool(
    call_name: &str,
    call_arguments: serde_json::Value,
    tool_call_id: Option<&str>,
    dispatch: ToolDispatchContext<'_>,
    meta: &TurnMeta<'_>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
    event_tx: Option<&Sender<TurnEvent>>,
) -> Result<ToolExecutionOutcome> {
    let full_args = call_arguments.to_string();
    let tool_call_id_owned = tool_call_id.map(str::to_string);
    observer.record_event(&ObserverEvent::ToolCallStart {
        tool: call_name.to_string(),
        tool_call_id: tool_call_id_owned.clone(),
        arguments: Some(full_args.clone()),
        channel: Some(meta.channel_name.to_string()),
        agent_alias: meta.agent_alias.map(|s| s.to_string()),
        parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
        turn_id: Some(meta.turn_id.to_string()),
    });
    let start = Instant::now();

    if is_excluded_tool(call_name, dispatch.excluded_tools) {
        return Ok(unavailable_tool_outcome(
            call_name,
            tool_call_id_owned,
            &full_args,
            meta,
            observer,
            start.elapsed(),
        ));
    }

    let static_tool = find_tool(dispatch.tools_registry, call_name);
    let activated_arc = if static_tool.is_none() {
        match dispatch.activated_tools {
            Some(at) => {
                let activated_tools = match at.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "tool": call_name,
                                "tool_call_id": tool_call_id,
                            })),
                            "activated-tool lock poisoned while resolving tool; recovering guard for read"
                        );
                        poisoned.into_inner()
                    }
                };
                activated_tools.get_resolved(call_name)
            }
            None => None,
        }
    } else {
        None
    };
    let Some(tool) = static_tool.or(activated_arc.as_deref()) else {
        let reason = format!("Unknown tool: {call_name}");
        let duration = start.elapsed();
        observer.record_event(&ObserverEvent::ToolCall {
            tool: call_name.to_string(),
            tool_call_id: tool_call_id_owned.clone(),
            duration,
            success: false,
            arguments: Some(full_args.clone()),
            result: Some(scrub_credentials(&reason)),
            channel: Some(meta.channel_name.to_string()),
            agent_alias: meta.agent_alias.map(|s| s.to_string()),
            parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
            turn_id: Some(meta.turn_id.to_string()),
        });
        return Ok(ToolExecutionOutcome {
            output: reason.clone(),
            success: false,
            error_reason: Some(reason),
            duration,
            receipt: None,
            output_data: None,
        });
    };

    // Request-scoped tool gate: when a chat-completions request narrows the
    // tool set with `tools: [...]`, reject tools that are in the agent's
    // registry but outside the request scope. On default (non-scoped) paths
    // this is `None` — the full agent tool set is available.
    if let Some(request_tools) = dispatch.request_tool_names
        && !request_tools.contains(&call_name.to_ascii_lowercase())
    {
        let reason = format!("Tool not available: {call_name}");
        let duration = start.elapsed();
        observer.record_event(&ObserverEvent::ToolCall {
            tool: call_name.to_string(),
            tool_call_id: tool_call_id_owned.clone(),
            duration,
            success: false,
            arguments: Some(full_args.clone()),
            result: Some(scrub_credentials(&reason)),
            channel: Some(meta.channel_name.to_string()),
            agent_alias: meta.agent_alias.map(|s| s.to_string()),
            parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
            turn_id: Some(meta.turn_id.to_string()),
        });
        return Ok(ToolExecutionOutcome {
            output: reason.clone(),
            success: false,
            error_reason: Some(reason),
            duration,
            receipt: None,
            output_data: None,
        });
    }

    if is_excluded_tool(tool.name(), dispatch.excluded_tools) {
        return Ok(unavailable_tool_outcome(
            call_name,
            tool_call_id_owned,
            &full_args,
            meta,
            observer,
            start.elapsed(),
        ));
    }

    use ::zeroclaw_log::Instrument;
    let tool_span = ::zeroclaw_log::info_span!(
        target: "zeroclaw_log_internal_scope",
        "zeroclaw_scope",
        tool = %call_name,
    );

    // Auto tool I/O propagation: emit Start with full input, run the
    // tool, then emit Complete or Fail with full output. Per-tool
    // execute() impls add zero logging.
    let _start_guard = tool_span.clone().entered();
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Invoke)
            .with_category(::zeroclaw_log::EventCategory::Tool)
            .with_attrs(::serde_json::json!({
                "tool": call_name,
                "tool_call_id": tool_call_id,
                "input": call_arguments,
            })),
        format!("tool call: {call_name}")
    );
    drop(_start_guard);

    // Stable correlation id for this call's pending ToolCall and terminal
    // ToolResult. Native calls carry their own id; id-less text-protocol calls
    // get one synthesized UUID reused for both halves so ACP/WS clients key the
    // tool_call_update to the right pending tool_call.
    let event_call_id = tool_call_id_owned
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Some(tx) = event_tx {
        let _ = tx
            .send(TurnEvent::ToolCall {
                id: event_call_id.clone(),
                name: call_name.to_string(),
                args: call_arguments.clone(),
            })
            .await;
    }

    let tool_future = tool
        .execute(call_arguments.clone())
        .instrument(tool_span.clone());
    let tool_result = if let Some(token) = cancellation_token {
        tokio::select! {
            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
            result = tool_future => result,
        }
    } else {
        tool_future.await
    };

    let outcome = {
        let _result_guard = tool_span.entered();
        match tool_result {
            Ok(r) => {
                let duration = start.elapsed();
                if r.success {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(
                            module_path!(),
                            ::zeroclaw_log::Action::Complete
                        )
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Success)
                        .with_duration(duration.as_millis() as u64)
                        .with_attrs(::serde_json::json!({
                            "tool": call_name,
                            "tool_call_id": tool_call_id,
                            "input": call_arguments,
                            "output": r.output,
                        })),
                        format!("tool result: {call_name}")
                    );
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_duration(duration.as_millis() as u64)
                            .with_attrs(::serde_json::json!({
                                "tool": call_name,
                                "tool_call_id": tool_call_id,
                                "input": call_arguments,
                                "error": r.error.clone().unwrap_or_default(),
                                "output": r.output,
                            })),
                        format!("tool failed: {call_name}")
                    );
                }
                if r.success {
                    let normalized_output = if r.output.is_empty() {
                        "(no output)"
                    } else {
                        &r.output
                    };
                    let receipt = receipt_generator.map(|receipt_gen| {
                        receipt_gen.generate_now(call_name, &call_arguments, normalized_output)
                    });
                    observer.record_event(&ObserverEvent::ToolCall {
                        tool: call_name.to_string(),
                        tool_call_id: tool_call_id_owned.clone(),
                        duration,
                        success: true,
                        arguments: Some(full_args.clone()),
                        result: Some(scrub_credentials(normalized_output)),
                        channel: Some(meta.channel_name.to_string()),
                        agent_alias: meta.agent_alias.map(|s| s.to_string()),
                        parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
                        turn_id: Some(meta.turn_id.to_string()),
                    });
                    Ok(ToolExecutionOutcome {
                        output: normalized_output.to_string(),
                        output_data: r.output.into_data(),
                        success: true,
                        error_reason: None,
                        duration,
                        receipt,
                    })
                } else {
                    let reason = r.error.unwrap_or_else(|| r.output.into_string());
                    observer.record_event(&ObserverEvent::ToolCall {
                        tool: call_name.to_string(),
                        tool_call_id: tool_call_id_owned.clone(),
                        duration,
                        success: false,
                        arguments: Some(full_args.clone()),
                        result: Some(scrub_credentials(&reason)),
                        channel: Some(meta.channel_name.to_string()),
                        agent_alias: meta.agent_alias.map(|s| s.to_string()),
                        parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
                        turn_id: Some(meta.turn_id.to_string()),
                    });
                    Ok(ToolExecutionOutcome {
                        output: format!("Error: {reason}"),
                        success: false,
                        error_reason: Some(reason),
                        duration,
                        receipt: None,
                        output_data: None,
                    })
                }
            }
            Err(e) => {
                let duration = start.elapsed();
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_duration(duration.as_millis() as u64)
                        .with_attrs(::serde_json::json!({
                            "tool": call_name,
                            "tool_call_id": tool_call_id,
                            "input": call_arguments,
                            "error": format!("{e:?}"),
                        })),
                    format!("tool error: {call_name}")
                );
                let reason = format!("Error executing {call_name}: {e}");
                observer.record_event(&ObserverEvent::ToolCall {
                    tool: call_name.to_string(),
                    tool_call_id: tool_call_id_owned.clone(),
                    duration,
                    success: false,
                    arguments: Some(full_args.clone()),
                    result: Some(scrub_credentials(&reason)),
                    channel: Some(meta.channel_name.to_string()),
                    agent_alias: meta.agent_alias.map(|s| s.to_string()),
                    parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
                    turn_id: Some(meta.turn_id.to_string()),
                });
                Ok(ToolExecutionOutcome {
                    output: reason.clone(),
                    success: false,
                    error_reason: Some(reason),
                    duration,
                    receipt: None,
                    output_data: None,
                })
            }
        }
    };

    if let Some(tx) = event_tx
        && let Ok(out) = &outcome
    {
        let _ = tx
            .send(TurnEvent::ToolResult {
                id: event_call_id.clone(),
                name: call_name.to_string(),
                output: scrub_credentials(&out.output),
            })
            .await;
    }

    // After the ToolResult card closes, publish the plan if this was a
    // successful TodoWrite. Whole-list replace; parse failures are
    // swallowed (the ToolResult already conveyed success/failure).
    if let Some(tx) = event_tx
        && let Ok(out) = &outcome
        && let Some(plan_event) = maybe_plan_event(call_name, out.success, &call_arguments)
    {
        let _ = tx.send(plan_event).await;
    }

    outcome
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

pub(crate) async fn execute_tools_parallel(
    tool_calls: &[ParsedToolCall],
    dispatch: ToolDispatchContext<'_>,
    meta: &TurnMeta<'_>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
    event_tx: Option<&Sender<TurnEvent>>,
) -> Result<Vec<Option<ToolExecutionOutcome>>> {
    let futures: Vec<_> = tool_calls
        .iter()
        .map(|call| {
            execute_one_tool(
                &call.name,
                call.arguments.clone(),
                call.tool_call_id.as_deref(),
                dispatch,
                meta,
                observer,
                cancellation_token,
                receipt_generator,
                event_tx,
            )
        })
        .collect();

    let results = futures_util::future::join_all(futures).await;
    let mut slots = Vec::with_capacity(results.len());
    for result in results {
        match result {
            Ok(outcome) => slots.push(Some(outcome)),
            Err(e) if is_tool_loop_cancelled(&e) => slots.push(None),
            Err(e) => return Err(e),
        }
    }
    Ok(slots)
}

// ── Sequential execution ─────────────────────────────────────────────────

pub(crate) async fn execute_tools_sequential(
    tool_calls: &[ParsedToolCall],
    dispatch: ToolDispatchContext<'_>,
    meta: &TurnMeta<'_>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
    event_tx: Option<&Sender<TurnEvent>>,
) -> Result<Vec<Option<ToolExecutionOutcome>>> {
    let mut slots: Vec<Option<ToolExecutionOutcome>> = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            break;
        }
        let outcome = match execute_one_tool(
            &call.name,
            call.arguments.clone(),
            call.tool_call_id.as_deref(),
            dispatch,
            meta,
            observer,
            cancellation_token,
            receipt_generator,
            event_tx,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(e) if is_tool_loop_cancelled(&e) => break,
            Err(e) => return Err(e),
        };
        slots.push(Some(outcome));
    }

    slots.resize_with(tool_calls.len(), || None);
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::{ToolDispatchContext, execute_one_tool};
    use crate::observability::noop::NoopObserver;
    use crate::tools::ActivatedToolSet;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use zeroclaw_api::tool::Tool;

    /// Minimal tool that records invocations. Used to verify that the
    /// poisoned-lock recovery path still resolves an activated tool and
    /// calls its execute method successfully.
    struct CountingTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    impl zeroclaw_api::attribution::Attributable for CountingTool {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-counting-tool"
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Counts executions for poisoned-lock tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            })
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult {
                success: true,
                output: "executed via poisoned lock recovery".into(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn execute_one_tool_recovers_poisoned_activated_tool_lock() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "docker-mcp__extract_text",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("docker-mcp__extract_text".into(), activated_tool);

        // Poison the mutex by panicking while holding the lock in a
        // separate thread.
        let poisoned = Arc::clone(&activated);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().expect("test mutex should lock");
            panic!("deliberately poison the activated-tools lock");
        })
        .join();

        // execute_one_tool must recover the poisoned lock and resolve
        // the activated tool without panicking.
        let meta = crate::agent::turn::TurnMeta {
            parent_agent_alias: None,
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let outcome = execute_one_tool(
            "docker-mcp__extract_text",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &[], // no static tools - force activated-tools path
                activated_tools: Some(&activated),
                excluded_tools: &[],
                request_tool_names: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should recover from poisoned lock");

        assert!(
            outcome.success,
            "activated tool execution should succeed after poisoned lock recovery"
        );
        assert!(
            outcome
                .output
                .contains("executed via poisoned lock recovery"),
            "tool output should come from the recovered activated tool"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "recovered activated tool should have been invoked exactly once"
        );
    }

    #[tokio::test]
    async fn execute_one_tool_blocks_excluded_activated_suffix_resolution() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "docker-mcp__extract_text",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("docker-mcp__extract_text".into(), activated_tool);

        let meta = crate::agent::turn::TurnMeta {
            parent_agent_alias: None,
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let excluded = vec!["docker-mcp__extract_text".to_string()];
        let outcome = execute_one_tool(
            "extract_text",
            serde_json::json!({}),
            Some("call-1"),
            ToolDispatchContext {
                tools_registry: &[],
                activated_tools: Some(&activated),
                excluded_tools: &excluded,
                request_tool_names: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("excluded activated tool should return an unavailable outcome");

        assert!(!outcome.success);
        assert_eq!(
            outcome.output,
            "Tool not available in this turn: extract_text"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 0);
    }

    // ── Regression: request_tool_names gate rejects out-of-scope tools ──

    #[tokio::test]
    async fn request_tool_names_gate_rejects_tool_outside_scope() {
        // Chat-completions with `tools: [weather_query]` must reject native
        // calls for tools in the registry but outside the request scope.
        // `find_tool` locates the tool but the gate returns "Tool not available".
        let invocations = Arc::new(AtomicUsize::new(0));
        let tool: Box<dyn Tool> = Box::new(CountingTool::new("shell", invocations.clone()));
        let meta = crate::agent::turn::TurnMeta {
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "http",
        };
        let mut allowed: std::collections::HashSet<String> = std::collections::HashSet::new();
        allowed.insert("weather_query".to_string());
        let outcome = execute_one_tool(
            "shell",
            serde_json::json!({"cmd": "rm -rf /"}),
            Some("call-1"),
            ToolDispatchContext {
                tools_registry: &[tool],
                activated_tools: None,
                excluded_tools: &[],
                request_tool_names: Some(&allowed),
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("request_tool_names gate should return an outcome, not panic");

        assert!(!outcome.success, "shell outside scope must be rejected");
        assert!(
            outcome.output.contains("Tool not available"),
            "gate must produce 'Tool not available', got: {}",
            outcome.output
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "rejected tool must not have executed"
        );
    }

    #[tokio::test]
    async fn request_tool_names_none_allows_all_registry_tools() {
        // Default paths (WS/A2A/webhook) pass None — the full registry is
        // available and any configured tool can execute.
        let invocations = Arc::new(AtomicUsize::new(0));
        let tool: Box<dyn Tool> = Box::new(CountingTool::new("shell", invocations.clone()));
        let meta = crate::agent::turn::TurnMeta {
            agent_alias: None,
            parent_agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "wss",
        };
        let outcome = execute_one_tool(
            "shell",
            serde_json::json!({"cmd": "echo hello"}),
            Some("call-1"),
            ToolDispatchContext {
                tools_registry: &[tool],
                activated_tools: None,
                excluded_tools: &[],
                request_tool_names: None, // default path
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("default path should allow any registered tool");

        assert!(
            outcome.success,
            "shell in registry must execute on default path"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "allowed tool must have executed exactly once"
        );
    }

    use super::should_execute_tools_in_parallel;
    use crate::agent::loop_::ParsedToolCall;
    use crate::approval::ApprovalManager;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::schema::RiskProfileConfig;

    fn parsed_tool_call(name: &str) -> ParsedToolCall {
        ParsedToolCall {
            name: name.to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: None,
        }
    }

    fn supervised_risk_profile() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            auto_approve: vec!["file_read".into()],
            always_ask: vec!["shell".into()],
            ..RiskProfileConfig::default()
        }
    }

    // --- tool_search branch---

    #[test]
    fn tool_search_in_batch_forces_serial() {
        // Two non-approval-gated tools in a batch where one is `tool_search`
        // must run sequentially. Without the `tool_search` branch the default
        // path would return `true` and the runtime would dispatch them in
        // parallel, racing the lookup against the activation.
        let calls = vec![
            parsed_tool_call("tool_search"),
            parsed_tool_call("file_read"),
        ];

        assert!(
            !should_execute_tools_in_parallel(&calls, None),
            "batch containing tool_search must force sequential execution (line 349-351)"
        );
    }

    #[test]
    fn tool_search_with_approval_required_in_batch_still_forces_serial() {
        // When both branches would trigger, the test only needs to confirm
        // the call still returns `false` — the ordering between the
        // `tool_search` branch and the approval branch is an implementation
        // detail. The important invariant is: `tool_search` present ⇒ serial.
        let calls = vec![parsed_tool_call("tool_search"), parsed_tool_call("shell")];
        let approval_cfg = zeroclaw_config::schema::RiskProfileConfig::default();
        let approval_mgr = ApprovalManager::from_risk_profile(&approval_cfg);

        assert!(
            !should_execute_tools_in_parallel(&calls, Some(&approval_mgr)),
            "tool_search in a mixed approval batch must still force sequential execution"
        );
    }

    #[test]
    fn non_search_non_approval_batch_remains_parallel_eligible() {
        let calls = vec![
            parsed_tool_call("file_read"),
            parsed_tool_call("memory_recall"),
        ];

        assert!(
            should_execute_tools_in_parallel(&calls, None),
            "non-tool_search, non-approval batch must remain parallel-eligible (default branch)"
        );
    }

    // --- approval-required + control branches---

    #[test]
    fn approval_required_batch_forces_sequential() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_risk_profile());
        let batch = vec![
            parsed_tool_call("file_read"),
            parsed_tool_call("shell"),
            parsed_tool_call("file_read"),
        ];
        assert!(
            !should_execute_tools_in_parallel(&batch, Some(&mgr)),
            "batch with approval-required tool must execute sequentially"
        );
    }

    #[test]
    fn approval_required_alone_in_batch_still_sequential() {
        // A two-element batch where one tool requires approval must still
        // take the serial branch (length check above already returns false
        // for len <= 1; this asserts the approval branch is the actual gate).
        let mgr = ApprovalManager::for_non_interactive(&supervised_risk_profile());
        let batch = vec![parsed_tool_call("file_read"), parsed_tool_call("shell")];
        assert!(
            !should_execute_tools_in_parallel(&batch, Some(&mgr)),
            "approval branch must trigger regardless of approval tool position"
        );
    }

    #[test]
    fn mixed_batch_with_approval_forces_serial_even_with_parallel_candidates() {
        // Mixed batch: two file_read (parallel candidates) plus one shell
        // (approval-required). The presence of `shell` must force serial
        // execution, even though the other two could otherwise run in
        // parallel.
        let mgr = ApprovalManager::for_non_interactive(&supervised_risk_profile());
        let batch = vec![
            parsed_tool_call("file_read"),
            parsed_tool_call("shell"),
            parsed_tool_call("file_read"),
        ];
        assert!(
            !should_execute_tools_in_parallel(&batch, Some(&mgr)),
            "mixed batch must serialize when any approval-required tool is present"
        );
    }

    #[test]
    fn parallel_when_no_approval_and_no_tool_search() {
        // Control case: a batch of three non-approval, non-tool_search
        // calls under `Supervised` (where `file_read` is auto-approved and
        // `shell` is approval-required) may run in parallel.
        let mgr = ApprovalManager::for_non_interactive(&supervised_risk_profile());
        let batch = vec![
            parsed_tool_call("file_read"),
            parsed_tool_call("file_read"),
            parsed_tool_call("file_read"),
        ];
        assert!(
            should_execute_tools_in_parallel(&batch, Some(&mgr)),
            "non-approval, non-tool_search batch must run in parallel when allowed"
        );
    }

    #[test]
    fn full_autonomy_batch_with_unknown_tool_runs_in_parallel() {
        // Under `Full` autonomy, no tool requires approval — `needs_approval`
        // returns false for every name. The control case extends to a batch
        // whose names would otherwise be unknown to supervised profile.
        let full = RiskProfileConfig {
            level: AutonomyLevel::Full,
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::for_non_interactive(&full);
        let batch = vec![
            parsed_tool_call("file_write"),
            parsed_tool_call("shell"),
            parsed_tool_call("anything"),
        ];
        assert!(
            should_execute_tools_in_parallel(&batch, Some(&mgr)),
            "full autonomy never prompts, so parallel execution is allowed"
        );
    }

    #[test]
    fn no_approval_manager_with_multi_call_batch_runs_in_parallel() {
        // When the caller passes `None` for `approval` and no tool in the
        // batch is `tool_search`, the function takes the parallel branch
        // unconditionally — useful for the tests / harnesses that exercise
        // the tool loop without an approval manager.
        let batch = vec![
            parsed_tool_call("file_read"),
            parsed_tool_call("memory_recall"),
        ];
        assert!(
            should_execute_tools_in_parallel(&batch, None),
            "no approval manager + non-tool_search batch must run in parallel"
        );
    }

    // ── Plan emission tests ────────────────────────────────────────────────

    #[cfg(test)]
    mod plan_emission_tests {
        use super::super::maybe_plan_event;
        use serde_json::json;

        #[test]
        fn plan_event_built_for_successful_todowrite() {
            let args = json!({ "todos": [ { "content": "A", "status": "pending" } ] });
            let ev = maybe_plan_event("TodoWrite", true, &args);
            match ev {
                Some(zeroclaw_api::agent::TurnEvent::Plan { entries }) => {
                    assert_eq!(entries.len(), 1);
                    assert_eq!(entries[0].content, "A");
                }
                _ => panic!("expected a Plan event"),
            }
        }

        #[test]
        fn no_plan_event_for_other_tools() {
            let args = json!({ "todos": [ { "content": "A", "status": "pending" } ] });
            assert!(maybe_plan_event("shell", true, &args).is_none());
        }

        #[test]
        fn no_plan_event_for_failed_todowrite() {
            let args = json!({ "todos": [ { "content": "A", "status": "pending" } ] });
            assert!(maybe_plan_event("TodoWrite", false, &args).is_none());
        }

        #[test]
        fn no_plan_event_for_unparseable_todowrite_args() {
            let args = json!({ "todos": [ { "status": "pending" } ] });
            assert!(maybe_plan_event("TodoWrite", true, &args).is_none());
        }

        #[test]
        fn empty_list_produces_clear_plan_event() {
            let args = json!({ "todos": [] });
            match maybe_plan_event("TodoWrite", true, &args) {
                Some(zeroclaw_api::agent::TurnEvent::Plan { entries }) => {
                    assert!(entries.is_empty());
                }
                _ => panic!("expected an empty Plan event (clear)"),
            }
        }
    }
}
