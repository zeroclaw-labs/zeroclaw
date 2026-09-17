//! Tool execution helpers extracted from `loop_`.

use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::approval::ApprovalManager;
use crate::observability::{Observer, ObserverEvent};
use crate::tools::{ActivatedToolSet, Tool};
use tokio::sync::mpsc::Sender;
use zeroclaw_api::agent::{ToolArtifact, TurnEvent};
use zeroclaw_api::attribution::Attributable;

// Items that still live in `loop_` — import via the parent module.
use super::loop_::{ParsedToolCall, ToolLoopCancelled, is_tool_loop_cancelled, scrub_credentials};
use super::turn::{ModelSwitchCallback, TurnMeta, scope_model_switch_state};

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

/// Resolve presentation provenance with the same static-then-activated lookup
/// order used by execution. Unknown names remain `None` so callers fail closed.
pub(crate) fn resolved_tool_provenance(
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&Arc<std::sync::Mutex<ActivatedToolSet>>>,
    name: &str,
) -> Option<zeroclaw_api::attribution::ToolProvenance> {
    if let Some(tool) = find_tool(tools_registry, name) {
        return Some(tool.tool_provenance());
    }

    activated_tools
        .map(|activated| match activated.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        })
        .and_then(|activated| {
            activated
                .get_resolved(name)
                .map(|tool| tool.tool_provenance())
        })
}

#[derive(Clone, Copy)]
pub(crate) struct ToolDispatchContext<'a> {
    pub tools_registry: &'a [Box<dyn Tool>],
    pub activated_tools: Option<&'a std::sync::Arc<std::sync::Mutex<ActivatedToolSet>>>,
    pub excluded_tools: &'a [String],
    pub model_switch_callback: Option<&'a ModelSwitchCallback>,
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
    /// Text handed to the model and persisted to provider history. The
    /// success path carries raw bytes; the failure paths of `execute_one_tool`
    /// fold a tool's detailed error body (which can reflect a token or signed
    /// URL) into this text and credential-scrub it before storing it here.
    pub output: String,
    /// Structured output when the tool declared one (`ToolOutput::data`).
    /// Feeds SOP step capture and data-flow surfaces; the LLM sees only
    /// `output`. Stored raw — consumers scrub at their own rendering boundary.
    pub output_data: Option<serde_json::Value>,
    pub success: bool,
    /// Raw, unscrubbed failure text for trusted in-process consumers (SOP step
    /// capture, data-flow surfaces). Credential scrubbing is a rendering
    /// concern applied at each human-facing surface (observer events,
    /// post-execution log line, CLI progress) and, unlike this field, on the
    /// model-visible `output`.
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
    let execute = async {
        if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => Err::<_, anyhow::Error>(ToolLoopCancelled.into()),
                result = tool_future => Ok(result),
            }
        } else {
            Ok(tool_future.await)
        }
    };
    let tool_result = if let Some(model_switch_callback) = dispatch.model_switch_callback {
        scope_model_switch_state(Arc::clone(model_switch_callback), execute).await
    } else {
        execute.await
    }?;

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
                    // A tool can report a short `error` (e.g. "HTTP 400") while
                    // separately building a richer `output` with the detail an
                    // agent would need to self-correct (e.g. the full response
                    // body explaining what was wrong with the request). Only
                    // `output` reaches the LLM (see `ToolExecutionOutcome::output`
                    // doc comment), so when both are present and distinct, fold
                    // the detail into what the agent sees instead of discarding
                    // it. Tools that already put everything into `error` and
                    // leave `output` empty (the common case) are unaffected.
                    let output_text = r.output.as_str().to_string();
                    let output_data = r.output.into_data();
                    let reason = r.error.unwrap_or_else(|| output_text.clone());
                    let full_output = if !output_text.is_empty() && output_text != reason {
                        format!("{reason}\n\n{output_text}")
                    } else {
                        reason.clone()
                    };
                    // Folding the tool's detailed `output` into the
                    // model-visible text is a credential-egress boundary: a
                    // failing remote call can echo a token or signed URL in
                    // its error body, and before this fold that body was
                    // discarded. Scrub the combined text once and share it
                    // with both the model-bound outcome and the observer
                    // event. `error_reason` and `output_data` stay raw for
                    // trusted in-process consumers (SOP step capture,
                    // data-flow surfaces) that scrub at their own rendering
                    // boundary.
                    let model_visible = scrub_credentials(&full_output);
                    observer.record_event(&ObserverEvent::ToolCall {
                        tool: call_name.to_string(),
                        tool_call_id: tool_call_id_owned.clone(),
                        duration,
                        success: false,
                        arguments: Some(full_args.clone()),
                        result: Some(model_visible.clone()),
                        channel: Some(meta.channel_name.to_string()),
                        agent_alias: meta.agent_alias.map(|s| s.to_string()),
                        parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
                        turn_id: Some(meta.turn_id.to_string()),
                    });
                    Ok(ToolExecutionOutcome {
                        output: format!("Error: {model_visible}"),
                        success: false,
                        error_reason: Some(reason),
                        duration,
                        receipt: None,
                        output_data,
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
                // Same model-visible egress boundary as the
                // `Ok(success = false)` arm above: a tool error can embed a
                // redirect URL with a signed query string. Scrub the
                // model-bound text; keep `error_reason` raw for trusted
                // in-process consumers.
                let model_visible = scrub_credentials(&reason);
                observer.record_event(&ObserverEvent::ToolCall {
                    tool: call_name.to_string(),
                    tool_call_id: tool_call_id_owned.clone(),
                    duration,
                    success: false,
                    arguments: Some(full_args.clone()),
                    result: Some(model_visible.clone()),
                    channel: Some(meta.channel_name.to_string()),
                    agent_alias: meta.agent_alias.map(|s| s.to_string()),
                    parent_agent_alias: meta.parent_agent_alias.map(|s| s.to_string()),
                    turn_id: Some(meta.turn_id.to_string()),
                });
                Ok(ToolExecutionOutcome {
                    output: model_visible,
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
                artifact: out
                    .output_data
                    .as_ref()
                    .and_then(ToolArtifact::from_delivered_data),
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
    use super::{
        Observer, ObserverEvent, ToolDispatchContext, execute_one_tool, resolved_tool_provenance,
    };
    use crate::observability::noop::NoopObserver;
    use crate::observability::traits::ObserverMetric;
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

    #[test]
    fn resolved_provenance_uses_activated_mcp_tool() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let invocations = Arc::new(AtomicUsize::new(0));
        let tool: Arc<dyn Tool> = Arc::new(CountingTool::new("mcp__browser", invocations));
        activated
            .lock()
            .unwrap()
            .activate("mcp__browser".into(), tool);

        assert_eq!(
            resolved_tool_provenance(&[], Some(&activated), "mcp__browser"),
            Some(zeroclaw_api::attribution::ToolProvenance::Extension)
        );
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
                tools_registry: &crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                    vec![],
                ), // no static tools - force activated-tools path
                activated_tools: Some(&activated),
                excluded_tools: &[],
                model_switch_callback: None,
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
                tools_registry: &crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                    vec![],
                ),
                activated_tools: Some(&activated),
                excluded_tools: &excluded,
                model_switch_callback: None,
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

    /// Fake tool that always fails, with an `error` distinct from `output` —
    /// mirrors `http_request`'s pattern of a short status in `error` plus a
    /// structured, detailed body in `output` built via
    /// `ToolOutput::json_with_text`, the same constructor
    /// `http_request.rs` uses for every 4xx/5xx response
    /// (`crates/zeroclaw-tools/src/http_request.rs:672-680`).
    struct FailingToolWithDetailedOutput;

    fn failing_tool_body_data() -> serde_json::Value {
        serde_json::json!({
            "status": 400,
            "reason": "Bad Request",
            "headers": "",
            "body": {
                "message": "the api-version needs the -preview suffix",
                "typeKey": "VssInvalidPreviewVersionException",
            },
        })
    }

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolWithDetailedOutput {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool"
        }
    }

    #[async_trait]
    impl Tool for FailingToolWithDetailedOutput {
        fn name(&self) -> &str {
            "failing_tool"
        }

        fn description(&self) -> &str {
            "Always fails with error + detailed output, for regression testing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: zeroclaw_api::tool::ToolOutput::json_with_text(
                    failing_tool_body_data(),
                    "Response Body: the api-version needs the -preview suffix",
                ),
                error: Some("HTTP 400".into()),
            })
        }
    }

    /// Fake tool that fails with only `error` set and empty `output` — the
    /// common case (e.g. `file_edit`, blocked shell commands) that must keep
    /// behaving exactly as before this change.
    struct FailingToolWithNoOutput;

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolWithNoOutput {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool-no-output"
        }
    }

    #[async_trait]
    impl Tool for FailingToolWithNoOutput {
        fn name(&self) -> &str {
            "failing_tool_no_output"
        }

        fn description(&self) -> &str {
            "Always fails with only error set, for regression testing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: zeroclaw_api::tool::ToolOutput::default(),
                error: Some("old_string not found in file".into()),
            })
        }
    }

    fn test_turn_meta() -> crate::agent::turn::TurnMeta<'static> {
        crate::agent::turn::TurnMeta {
            parent_agent_alias: None,
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        }
    }

    #[tokio::test]
    async fn execute_one_tool_includes_detailed_output_alongside_short_error() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolWithDetailedOutput)];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "failing_tool",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(!outcome.success);
        assert!(
            outcome.output.contains("HTTP 400"),
            "the short error must still be present: {}",
            outcome.output
        );
        assert!(
            outcome
                .output
                .contains("the api-version needs the -preview suffix"),
            "the tool's detailed output must reach the agent, not just the bare status: {}",
            outcome.output
        );
        assert_eq!(
            outcome.error_reason.as_deref(),
            Some("HTTP 400"),
            "error_reason stays the short, raw reason for other consumers"
        );
        assert_eq!(
            outcome.output_data,
            Some(failing_tool_body_data()),
            "structured output_data (the shape http_request.rs actually produces via \
             ToolOutput::json_with_text) must survive the failure path, not just the \
             display text"
        );
    }

    #[tokio::test]
    async fn execute_one_tool_error_only_output_is_unchanged() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolWithNoOutput)];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "failing_tool_no_output",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(!outcome.success);
        assert_eq!(
            outcome.output, "Error: old_string not found in file",
            "tools with empty output must keep the exact pre-existing message shape"
        );
    }

    /// The production-boundary case the linked issue explicitly requires:
    /// the real `http_request` tool against a live 4xx response, run through
    /// `execute_one_tool`, proving the response body it builds via
    /// `ToolOutput::json_with_text` (`http_request.rs:661-680`) reaches the
    /// agent-visible outcome rather than being replaced by the bare
    /// `"HTTP 400"` `error`. Mirrors the real-world Azure DevOps repro from
    /// the issue (a `-preview` api-version 400 response).
    #[tokio::test]
    async fn execute_one_tool_preserves_http_request_400_body_from_real_producer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed");
        let port = listener.local_addr().unwrap().port();

        let body = serde_json::json!({
            "message": "The requested version \"7.1\" of the resource is under preview. \
                The -preview flag must be supplied in the api-version for such requests.",
            "typeKey": "VssInvalidPreviewVersionException",
        })
        .to_string();

        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept must succeed");
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut buffer = [0_u8; 1024];
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("read must not error before headers complete");
                assert!(read > 0, "client closed before completing request headers");
                request.extend_from_slice(&buffer[..read]);
            }
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write must succeed");
        });

        let http_tool = zeroclaw_tools::http_request::HttpRequestTool::new(
            Arc::new(zeroclaw_config::policy::SecurityPolicy {
                autonomy: AutonomyLevel::Supervised,
                ..zeroclaw_config::policy::SecurityPolicy::default()
            }),
            vec!["127.0.0.1".into()],
            1_000_000,
            5,
            true,
            Vec::new(),
            Vec::new(),
        )
        .expect("HttpRequestTool::new must succeed with a valid allowlist");

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(http_tool)];
        let meta = test_turn_meta();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            execute_one_tool(
                "http_request",
                serde_json::json!({
                    "url": format!("http://127.0.0.1:{port}/_apis/connectionData?api-version=7.1"),
                    "method": "GET",
                }),
                None,
                ToolDispatchContext {
                    tools_registry: &tools,
                    activated_tools: None,
                    excluded_tools: &[],
                    model_switch_callback: None,
                },
                &meta,
                &NoopObserver,
                None,
                None,
                None,
            ),
        )
        .await
        .expect("execute_one_tool must not hang against the loopback server")
        .expect("execute_one_tool should return an outcome for the real http_request tool");

        assert!(!outcome.success);
        assert!(
            outcome.output.contains("HTTP 400"),
            "the short error must still be present: {}",
            outcome.output
        );
        assert!(
            outcome
                .output
                .contains("The -preview flag must be supplied"),
            "the real response body from http_request must reach the agent, not just \
             the bare status: {}",
            outcome.output
        );
        let output_data = outcome
            .output_data
            .expect("http_request's structured body must survive the failure path");
        assert_eq!(
            output_data["status"], 400,
            "structured data must carry the real status code: {output_data}"
        );
        assert_eq!(
            output_data["body"]["typeKey"], "VssInvalidPreviewVersionException",
            "structured data must carry the parsed JSON body http_request produced: {output_data}"
        );
    }

    /// Fake tool whose detailed `output` is plain text with no structured
    /// `data` attached — the shape of a shell-like tool, as distinct from
    /// `http_request`'s `json_with_text`. Guards the branch that appends
    /// distinct plain text without ever synthesizing an `output_data`.
    struct FailingToolWithPlainTextOutput;

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolWithPlainTextOutput {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool-plain-text"
        }
    }

    #[async_trait]
    impl Tool for FailingToolWithPlainTextOutput {
        fn name(&self) -> &str {
            "failing_tool_plain_text"
        }

        fn description(&self) -> &str {
            "Always fails with a plain-text detailed output and no structured data, \
             for regression testing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: "stdout: connection refused while reaching upstream".into(),
                error: Some("exit code 1".into()),
            })
        }
    }

    #[tokio::test]
    async fn execute_one_tool_appends_plain_text_output_without_data() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolWithPlainTextOutput)];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "failing_tool_plain_text",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(!outcome.success);
        assert!(
            outcome.output.contains("exit code 1"),
            "the short error must still be present: {}",
            outcome.output
        );
        assert!(
            outcome
                .output
                .contains("connection refused while reaching upstream"),
            "plain-text detailed output (no structured data) must still reach the agent, \
             not just tools that happen to use json_with_text: {}",
            outcome.output
        );
        assert_eq!(
            outcome.output_data, None,
            "a tool that never declared structured data must not gain output_data \
             from this code path"
        );
    }

    /// Fake tool whose `output` text is byte-identical to its `error` —
    /// guards against re-introducing duplication like
    /// `"Error: HTTP 400\n\nHTTP 400"`.
    struct FailingToolWithOutputIdenticalToError;

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolWithOutputIdenticalToError {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool-identical-output"
        }
    }

    #[async_trait]
    impl Tool for FailingToolWithOutputIdenticalToError {
        fn name(&self) -> &str {
            "failing_tool_identical_output"
        }

        fn description(&self) -> &str {
            "Always fails with output text identical to its error, for regression testing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: "HTTP 400".into(),
                error: Some("HTTP 400".into()),
            })
        }
    }

    #[tokio::test]
    async fn execute_one_tool_does_not_duplicate_output_identical_to_error() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolWithOutputIdenticalToError)];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "failing_tool_identical_output",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(!outcome.success);
        assert_eq!(
            outcome.output, "Error: HTTP 400",
            "output text identical to error must not be duplicated: {}",
            outcome.output
        );
    }

    /// Fake tool that fails with `error: None` and only `output` set — the
    /// exact fallback line the original bug lived on
    /// (`r.error.unwrap_or_else(|| r.output.into_string())`). No existing
    /// test exercised the `None` arm of that closure post-fix.
    struct FailingToolWithNoErrorField;

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolWithNoErrorField {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool-no-error-field"
        }
    }

    #[async_trait]
    impl Tool for FailingToolWithNoErrorField {
        fn name(&self) -> &str {
            "failing_tool_no_error_field"
        }

        fn description(&self) -> &str {
            "Always fails with only `output` set and `error: None`, for regression testing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: "validation failed: field 'name' is required".into(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn execute_one_tool_error_none_falls_back_to_output_text() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolWithNoErrorField)];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "failing_tool_no_error_field",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(!outcome.success);
        assert_eq!(
            outcome.output, "Error: validation failed: field 'name' is required",
            "with error: None, the pre-existing fallback to r.output must be preserved \
             verbatim and not duplicated: {}",
            outcome.output
        );
        assert_eq!(
            outcome.error_reason.as_deref(),
            Some("validation failed: field 'name' is required"),
            "error_reason must fall back to the output text when the tool never set error"
        );
    }

    /// Captures the last `ObserverEvent::ToolCall.result` seen, so tests can
    /// assert on exactly what the observer/telemetry path receives — as
    /// distinct from what `ToolExecutionOutcome.output` sends to the model.
    struct RecordingObserver {
        last_result: Mutex<Option<String>>,
    }

    impl RecordingObserver {
        fn new() -> Self {
            Self {
                last_result: Mutex::new(None),
            }
        }

        fn last_result(&self) -> Option<String> {
            self.last_result.lock().unwrap().clone()
        }
    }

    impl Observer for RecordingObserver {
        fn record_event(&self, event: &ObserverEvent) {
            if let ObserverEvent::ToolCall { result, .. } = event {
                *self.last_result.lock().unwrap() = result.clone();
            }
        }

        fn record_metric(&self, _metric: &ObserverMetric) {}

        fn name(&self) -> &str {
            "recording-test-observer"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Fake tool whose detailed `output` embeds a credential-shaped string,
    /// mirroring a server that echoes back a bad `Authorization` header or
    /// API key in its error body.
    struct FailingToolWithSecretInOutput;

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolWithSecretInOutput {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool-secret"
        }
    }

    #[async_trait]
    impl Tool for FailingToolWithSecretInOutput {
        fn name(&self) -> &str {
            "failing_tool_secret"
        }

        fn description(&self) -> &str {
            "Always fails with a credential-shaped string in its detailed output, \
             for regression testing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: "Response Body: API_KEY=sk-1234567890abcdef was rejected".into(),
                error: Some("HTTP 401".into()),
            })
        }
    }

    /// Regression for the second Core Team review (`CHANGES_REQUESTED`):
    /// folding a tool's detailed failure body into the model-visible text is a
    /// credential-egress boundary. A failing remote call can echo a token or
    /// signed URL in its error body, and before this fold that body was
    /// discarded. The combined text must be credential-scrubbed before it is
    /// stored in `ToolExecutionOutcome.output` (and forwarded to the model /
    /// provider history), matching the scrub the observer event already
    /// applied — while the useful non-secret diagnostic still survives.
    #[tokio::test]
    async fn execute_one_tool_scrubs_credential_from_model_visible_failure_output() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolWithSecretInOutput)];
        let meta = test_turn_meta();
        let observer = RecordingObserver::new();
        let outcome = execute_one_tool(
            "failing_tool_secret",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &observer,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(!outcome.success);
        assert!(
            !outcome.output.contains("sk-1234567890abcdef"),
            "the model-visible failure output must be credential-scrubbed: {}",
            outcome.output
        );
        assert!(
            outcome.output.contains("[REDACTED]"),
            "expected the redaction marker in the model-visible output: {}",
            outcome.output
        );
        assert!(
            outcome.output.contains("HTTP 401") && outcome.output.contains("was rejected"),
            "scrubbing must not destroy the useful error context: {}",
            outcome.output
        );
        assert_eq!(
            outcome.error_reason.as_deref(),
            Some("HTTP 401"),
            "error_reason keeps the short, raw reason for trusted in-process consumers"
        );

        let observer_result = observer
            .last_result()
            .expect("the ToolCall event must carry a result for a failed call");
        assert!(
            !observer_result.contains("sk-1234567890abcdef")
                && observer_result.contains("[REDACTED]"),
            "the observer/telemetry event stays scrubbed too: {observer_result}"
        );
    }

    /// Fake failing tool with a caller-supplied `ToolOutput` and `error`, so a
    /// single test can drive every `error`/`output` combination the failure
    /// arm branches on.
    struct ConfigurableFailingTool {
        output: zeroclaw_api::tool::ToolOutput,
        error: Option<String>,
    }

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for ConfigurableFailingTool {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-configurable-failing-tool"
        }
    }

    #[async_trait]
    impl Tool for ConfigurableFailingTool {
        fn name(&self) -> &str {
            "configurable_failing_tool"
        }

        fn description(&self) -> &str {
            "Fails with a caller-supplied output/error, for failure-path scrubbing tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: self.output.clone(),
                error: self.error.clone(),
            })
        }
    }

    /// Every `error`/`output` shape the failure arm branches on, each carrying
    /// a credential-shaped value that must not survive into the model-visible
    /// `outcome.output` while its surrounding non-secret diagnostic must.
    ///
    /// Coverage inventory for the model/provider-history egress of a failed
    /// tool's content (the only two runtime sites that carry tool-supplied
    /// text into `ToolExecutionOutcome.output`):
    ///   - `Ok(ToolResult { success: false, .. })` arm — this test + the
    ///     real-`http_request` test below.
    ///   - `Err(e)` arm — `execute_one_tool_err_branch_scrubs_model_visible_credential`.
    /// Every other outcome constructor (`unavailable`/`unknown tool`, dedup,
    /// hook-cancel, approval-deny, interrupted) builds `output` from a static
    /// template or an operator-controlled string, never remote content.
    #[tokio::test]
    async fn execute_one_tool_scrubs_model_visible_credential_across_failure_shapes() {
        use zeroclaw_api::tool::ToolOutput;

        struct Case {
            name: &'static str,
            output: ToolOutput,
            error: Option<&'static str>,
            secret: &'static str,
            keep: &'static str,
            secret_in_error_reason: bool,
        }

        let cases = vec![
            Case {
                name: "concat: credential in the detailed body",
                output: ToolOutput::text("response body: token=sk-AAAA1111BBBB2222 rejected"),
                error: Some("HTTP 400"),
                secret: "sk-AAAA1111BBBB2222",
                keep: "rejected",
                secret_in_error_reason: false,
            },
            Case {
                name: "concat: credential in the short error",
                output: ToolOutput::text("consult the docs for the -preview suffix"),
                error: Some("blocked: api_key=sk-CCCC3333DDDD4444"),
                secret: "sk-CCCC3333DDDD4444",
                keep: "-preview suffix",
                secret_in_error_reason: true,
            },
            Case {
                name: "error set, output empty",
                output: ToolOutput::default(),
                error: Some("auth failed: password=hunter2-aaaaaaaa invalid"),
                secret: "hunter2-aaaaaaaa",
                keep: "auth failed",
                secret_in_error_reason: true,
            },
            Case {
                name: "output text identical to error",
                output: ToolOutput::text("token=sk-EEEE5555FFFF6666"),
                error: Some("token=sk-EEEE5555FFFF6666"),
                secret: "sk-EEEE5555FFFF6666",
                keep: "token=",
                secret_in_error_reason: true,
            },
            Case {
                name: "error: None, fall back to output text",
                output: ToolOutput::text("validation failed: secret=sk-GGGG7777HHHH8888"),
                error: None,
                secret: "sk-GGGG7777HHHH8888",
                keep: "validation failed",
                secret_in_error_reason: true,
            },
            Case {
                name: "json-only output (display text is the JSON)",
                output: ToolOutput::json(serde_json::json!({
                    "apikey": "sk-IIII9999JJJJ0000",
                    "hint": "add the -preview suffix",
                })),
                error: Some("HTTP 403"),
                secret: "sk-IIII9999JJJJ0000",
                keep: "-preview suffix",
                secret_in_error_reason: false,
            },
        ];

        for case in cases {
            let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigurableFailingTool {
                output: case.output.clone(),
                error: case.error.map(|e| e.to_string()),
            })];
            let meta = test_turn_meta();
            let observer = RecordingObserver::new();
            let outcome = execute_one_tool(
                "configurable_failing_tool",
                serde_json::json!({}),
                None,
                ToolDispatchContext {
                    tools_registry: &tools,
                    activated_tools: None,
                    excluded_tools: &[],
                    model_switch_callback: None,
                },
                &meta,
                &observer,
                None,
                None,
                None,
            )
            .await
            .expect("execute_one_tool should return an outcome for a failing tool");

            assert!(!outcome.success, "[{}]", case.name);
            assert!(
                !outcome.output.contains(case.secret),
                "[{}] model-visible output leaked the raw credential: {}",
                case.name,
                outcome.output
            );
            assert!(
                outcome.output.contains("[REDACTED]"),
                "[{}] model-visible output is missing the redaction marker: {}",
                case.name,
                outcome.output
            );
            assert!(
                outcome.output.contains(case.keep),
                "[{}] the non-secret diagnostic did not survive scrubbing: {}",
                case.name,
                outcome.output
            );

            let observer_result = observer
                .last_result()
                .expect("the ToolCall event must carry a result for a failed call");
            assert!(
                !observer_result.contains(case.secret),
                "[{}] observer event leaked the raw credential: {observer_result}",
                case.name
            );

            if case.secret_in_error_reason {
                assert!(
                    outcome
                        .error_reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains(case.secret)),
                    "[{}] error_reason must stay raw for trusted in-process consumers: {:?}",
                    case.name,
                    outcome.error_reason
                );
            }
        }
    }

    /// Fake tool whose `execute` returns `Err`, exercising the `Err(e)` arm of
    /// `execute_one_tool` (as distinct from `Ok(ToolResult { success: false })`).
    /// A tool error can embed a redirect URL with a signed query string.
    struct FailingToolReturningErr {
        message: &'static str,
    }

    #[async_trait]
    impl zeroclaw_api::attribution::Attributable for FailingToolReturningErr {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::System
        }
        fn alias(&self) -> &str {
            "test-failing-tool-returning-err"
        }
    }

    #[async_trait]
    impl Tool for FailingToolReturningErr {
        fn name(&self) -> &str {
            "failing_tool_returning_err"
        }

        fn description(&self) -> &str {
            "Returns Err from execute, for failure-path scrubbing tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}, "required": []})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Err(anyhow::Error::msg(self.message))
        }
    }

    #[tokio::test]
    async fn execute_one_tool_err_branch_scrubs_model_visible_credential() {
        for (message, secret, keep) in [
            (
                "connect failed for https://cb.example/r?token=abcd1234efgh5678ijkl",
                "abcd1234efgh5678ijkl",
                "connect failed",
            ),
            (
                "upstream rejected api_key=sk-DEADBEEF12345678",
                "sk-DEADBEEF12345678",
                "upstream rejected",
            ),
        ] {
            let tools: Vec<Box<dyn Tool>> = vec![Box::new(FailingToolReturningErr { message })];
            let meta = test_turn_meta();
            let observer = RecordingObserver::new();
            let outcome = execute_one_tool(
                "failing_tool_returning_err",
                serde_json::json!({}),
                None,
                ToolDispatchContext {
                    tools_registry: &tools,
                    activated_tools: None,
                    excluded_tools: &[],
                    model_switch_callback: None,
                },
                &meta,
                &observer,
                None,
                None,
                None,
            )
            .await
            .expect("execute_one_tool must still return an outcome when the tool errors");

            assert!(!outcome.success);
            assert!(
                !outcome.output.contains(secret),
                "Err-branch model-visible output leaked a credential: {}",
                outcome.output
            );
            assert!(
                outcome.output.contains("[REDACTED]"),
                "Err-branch output missing the redaction marker: {}",
                outcome.output
            );
            assert!(
                outcome.output.contains(keep)
                    && outcome
                        .output
                        .contains("Error executing failing_tool_returning_err"),
                "Err-branch output lost its diagnostic shape: {}",
                outcome.output
            );
            let observer_result = observer
                .last_result()
                .expect("the ToolCall event must carry a result for a failed call");
            assert!(
                !observer_result.contains(secret),
                "Err-branch observer event leaked a credential: {observer_result}"
            );
        }
    }

    /// Production-boundary companion to
    /// `execute_one_tool_preserves_http_request_400_body_from_real_producer`:
    /// the real `http_request` tool against a 4xx response whose JSON body
    /// reflects a credential. The reflected token must not reach the
    /// model-visible outcome; the actionable message must; and the structured
    /// `output_data` (which never reaches the model) deliberately keeps the
    /// raw parsed body for trusted consumers.
    #[tokio::test]
    async fn execute_one_tool_real_http_request_scrubs_credential_in_400_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback bind must succeed");
        let port = listener.local_addr().unwrap().port();

        let body = serde_json::json!({
            "message": "The supplied token is invalid; request a new one.",
            "token": "ghs_aAbBcCdDeEfFgGhHiIjJkKlLmMnN",
        })
        .to_string();

        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept must succeed");
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut buffer = [0_u8; 1024];
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("read must not error before headers complete");
                assert!(read > 0, "client closed before completing request headers");
                request.extend_from_slice(&buffer[..read]);
            }
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write must succeed");
        });

        let http_tool = zeroclaw_tools::http_request::HttpRequestTool::new(
            Arc::new(zeroclaw_config::policy::SecurityPolicy {
                autonomy: AutonomyLevel::Supervised,
                ..zeroclaw_config::policy::SecurityPolicy::default()
            }),
            vec!["127.0.0.1".into()],
            1_000_000,
            5,
            true,
            Vec::new(),
            Vec::new(),
        )
        .expect("HttpRequestTool::new must succeed with a valid allowlist");

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(http_tool)];
        let meta = test_turn_meta();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            execute_one_tool(
                "http_request",
                serde_json::json!({
                    "url": format!("http://127.0.0.1:{port}/_apis/connectionData?api-version=7.1"),
                    "method": "GET",
                }),
                None,
                ToolDispatchContext {
                    tools_registry: &tools,
                    activated_tools: None,
                    excluded_tools: &[],
                    model_switch_callback: None,
                },
                &meta,
                &NoopObserver,
                None,
                None,
                None,
            ),
        )
        .await
        .expect("execute_one_tool must not hang against the loopback server")
        .expect("execute_one_tool should return an outcome for the real http_request tool");

        assert!(!outcome.success);
        assert!(
            outcome.output.contains("HTTP 400"),
            "the short status must survive: {}",
            outcome.output
        );
        assert!(
            outcome.output.contains("The supplied token is invalid"),
            "the actionable message must survive scrubbing: {}",
            outcome.output
        );
        assert!(
            !outcome.output.contains("ghs_aAbBcCdDeEfFgGhHiIjJkKlLmMnN"),
            "the reflected token must be scrubbed from the model-visible output: {}",
            outcome.output
        );
        assert!(
            outcome.output.contains("[REDACTED]"),
            "expected the redaction marker: {}",
            outcome.output
        );

        let data = outcome
            .output_data
            .expect("http_request builds structured data even on a 4xx");
        assert_eq!(
            data["body"]["token"], "ghs_aAbBcCdDeEfFgGhHiIjJkKlLmMnN",
            "structured output_data does not reach the model and stays raw for trusted \
             consumers (which scrub at their own boundary): {data}"
        );
    }

    /// Characterization: `scrub_credentials` is a shared best-effort scrubber
    /// keyed off `name<sep>value` pairs. It does not cover `Authorization:
    /// Bearer <token>` (the space after `Bearer` breaks the value match). This
    /// gap predates this change; pinning it keeps the PR's security claim
    /// precise and makes any future tightening of the regex a visible change.
    #[tokio::test]
    async fn failure_output_scrub_leaves_bearer_prefixed_token_but_still_runs() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigurableFailingTool {
            output: zeroclaw_api::tool::ToolOutput::text(
                "upstream said: Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig ; \
                 api_key=sk-CATCHME01234567",
            ),
            error: Some("HTTP 401".into()),
        })];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "configurable_failing_tool",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(
            outcome.output.contains("eyJhbGciOiJIUzI1NiJ9.payload.sig"),
            "known gap: Bearer-prefixed tokens are not covered by the shared scrubber: {}",
            outcome.output
        );
        assert!(
            !outcome.output.contains("sk-CATCHME01234567") && outcome.output.contains("[REDACTED]"),
            "the scrubber still runs: the adjacent api_key pair is redacted: {}",
            outcome.output
        );
    }

    /// Characterization: signed-URL query parameters (`sig=`,
    /// `X-Amz-Signature=`) are not credential key names in the shared
    /// scrubber, so they pass through. Pre-existing gap; pinned for the same
    /// reason as the Bearer case above.
    #[tokio::test]
    async fn failure_output_scrub_leaves_signed_url_query_params_but_still_runs() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigurableFailingTool {
            output: zeroclaw_api::tool::ToolOutput::text(
                "redirect target: \
                 https://acct.blob.core.windows.net/c/b?sig=aBcD1234eFgH5678iJkL&se=2026 ; \
                 token=sk-CATCHME01234567",
            ),
            error: Some("HTTP 400".into()),
        })];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "configurable_failing_tool",
            serde_json::json!({}),
            None,
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");

        assert!(
            outcome.output.contains("sig=aBcD1234eFgH5678iJkL"),
            "known gap: signed-URL params are not covered by the shared scrubber: {}",
            outcome.output
        );
        assert!(
            !outcome.output.contains("sk-CATCHME01234567") && outcome.output.contains("[REDACTED]"),
            "the scrubber still runs: the adjacent token pair is redacted: {}",
            outcome.output
        );
    }

    /// End-to-end: a failed tool whose detailed body carries a credential must
    /// reach the model's tool-result message scrubbed, in both the native
    /// (`role=tool`) and the prompt-mode (`[Tool results]`) history shapes —
    /// the actual "text sent to the model / provider history".
    #[tokio::test]
    async fn failed_tool_credential_is_scrubbed_in_provider_history() {
        use crate::agent::loop_detector::{LoopDetector, LoopDetectorConfig};
        use crate::agent::turn::history_append::append_tool_round_to_history;
        use crate::agent::turn::results_collect::collect_tool_results;
        use std::collections::HashSet;
        use zeroclaw_providers::ChatMessage;

        let secret = "sk-HISTORY0123456789";
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(ConfigurableFailingTool {
            output: zeroclaw_api::tool::ToolOutput::text(format!(
                "response body: api_key={secret} was rejected"
            )),
            error: Some("HTTP 401".into()),
        })];
        let meta = test_turn_meta();
        let outcome = execute_one_tool(
            "configurable_failing_tool",
            serde_json::json!({}),
            Some("call-1"),
            ToolDispatchContext {
                tools_registry: &tools,
                activated_tools: None,
                excluded_tools: &[],
                model_switch_callback: None,
            },
            &meta,
            &NoopObserver,
            None,
            None,
            None,
        )
        .await
        .expect("execute_one_tool should return an outcome for a failing tool");
        assert!(
            !outcome.output.contains(secret),
            "precondition: outcome.output must already be scrubbed"
        );

        let tool_calls = vec![ParsedToolCall {
            name: "configurable_failing_tool".to_string(),
            arguments: serde_json::json!({}),
            tool_call_id: Some("call-1".to_string()),
        }];
        let ordered = vec![Some((
            "configurable_failing_tool".to_string(),
            Some("call-1".to_string()),
            outcome,
        ))];
        let mut history: Vec<ChatMessage> = Vec::new();
        let mut detector = LoopDetector::new(LoopDetectorConfig::default());
        let ignore: HashSet<&str> = HashSet::new();
        let collected = collect_tool_results(
            ordered,
            &tool_calls,
            &mut history,
            &mut detector,
            &ignore,
            0,
            None,
            "test-model",
            0,
            "turn-test",
        )
        .expect("collect_tool_results must succeed");

        assert!(
            !collected.tool_results.contains(secret)
                && collected.tool_results.contains("[REDACTED]"),
            "prompt-mode <tool_result> block must be scrubbed: {}",
            collected.tool_results
        );
        for (_, result) in &collected.individual_results {
            assert!(
                !result.contains(secret) && result.contains("[REDACTED]"),
                "native role=tool content must be scrubbed: {result}"
            );
        }

        let native_calls: Vec<zeroclaw_providers::ToolCall> = Vec::new();
        let mut native_history: Vec<ChatMessage> = Vec::new();
        append_tool_round_to_history(
            &mut native_history,
            "assistant text".to_string(),
            &native_calls,
            &collected.individual_results,
            &collected.tool_results,
            true,
        );
        assert!(
            native_history.iter().all(|m| !m.content.contains(secret)),
            "no native history message may carry the raw credential"
        );
        assert!(
            native_history
                .iter()
                .any(|m| m.content.contains("[REDACTED]")),
            "the native tool-result message must carry the scrubbed body"
        );

        let prompt_results = vec![(None, collected.individual_results[0].1.clone())];
        let mut prompt_history: Vec<ChatMessage> = Vec::new();
        append_tool_round_to_history(
            &mut prompt_history,
            "assistant text".to_string(),
            &native_calls,
            &prompt_results,
            &collected.tool_results,
            false,
        );
        assert!(
            prompt_history.iter().all(|m| !m.content.contains(secret)),
            "no prompt-mode history message may carry the raw credential"
        );
        assert!(
            prompt_history
                .iter()
                .any(|m| m.content.contains("[REDACTED]")),
            "the prompt-mode [Tool results] message must carry the scrubbed body"
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
