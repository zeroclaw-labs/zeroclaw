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
use tokio::sync::mpsc::Sender;
use zeroclaw_api::agent::TurnEvent;

// Items that still live in `loop_` — import via the parent module.
use super::loop_::{ParsedToolCall, ToolLoopCancelled, is_tool_loop_cancelled, scrub_credentials};
use super::turn::TurnMeta;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Look up a tool by name in a slice of boxed `dyn Tool` values.
pub fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().find(|t| t.name() == name).map(|t| t.as_ref())
}

// ── Outcome ──────────────────────────────────────────────────────────────

pub struct ToolExecutionOutcome {
    pub output: String,
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
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    meta: &TurnMeta<'_>,
    observer: &dyn Observer,
    cancellation_token: Option<&CancellationToken>,
    receipt_generator: Option<&super::tool_receipts::ReceiptGenerator>,
    event_tx: Option<&Sender<TurnEvent>>,
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
        channel: Some(meta.channel_name.to_string()),
        agent_alias: meta.agent_alias.map(|s| s.to_string()),
        turn_id: Some(meta.turn_id.to_string()),
    });
    let start = Instant::now();

    let static_tool = find_tool(tools_registry, call_name);
    let activated_arc = if static_tool.is_none() {
        match activated_tools {
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
            turn_id: Some(meta.turn_id.to_string()),
        });
        return Ok(ToolExecutionOutcome {
            output: reason.clone(),
            success: false,
            error_reason: Some(reason),
            duration,
            receipt: None,
        });
    };

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

    // Emit the pending ToolCall at the moment of dispatch, before the tool
    // future runs and potentially blocks. ACP/WS clients render this as the
    // live "running" card; without a pre-execution emit a long-running tool
    // leaves the turn visibly idle with no card until its result lands. The
    // terminal ToolResult below reuses this id to close the card. Serial
    // dispatch emits one pending per call in turn; parallel emits all pendings
    // as the futures spin up together.
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
                        turn_id: Some(meta.turn_id.to_string()),
                    });
                    Ok(ToolExecutionOutcome {
                        output: normalized_output.to_string(),
                        success: true,
                        error_reason: None,
                        duration,
                        receipt,
                    })
                } else {
                    let reason = r.error.unwrap_or(r.output);
                    observer.record_event(&ObserverEvent::ToolCall {
                        tool: call_name.to_string(),
                        tool_call_id: tool_call_id_owned.clone(),
                        duration,
                        success: false,
                        arguments: Some(full_args.clone()),
                        result: Some(scrub_credentials(&reason)),
                        channel: Some(meta.channel_name.to_string()),
                        agent_alias: meta.agent_alias.map(|s| s.to_string()),
                        turn_id: Some(meta.turn_id.to_string()),
                    });
                    Ok(ToolExecutionOutcome {
                        output: format!("Error: {reason}"),
                        success: false,
                        error_reason: Some(reason),
                        duration,
                        receipt: None,
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
                    turn_id: Some(meta.turn_id.to_string()),
                });
                Ok(ToolExecutionOutcome {
                    output: reason.clone(),
                    success: false,
                    error_reason: Some(reason),
                    duration,
                    receipt: None,
                })
            }
        }
    };

    // Emit the terminal ToolResult immediately after this call completes so
    // serial dispatch interleaves call->result per tool; the pending was
    // emitted before execution. Reuses the pending id to close the same card.
    // Cancelled-in-flight calls return early above and are closed by the turn
    // layer instead.
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

/// Concurrent batch executor. Returns one slot per call: `Some` when the call
/// completed and already emitted its terminal `ToolResult`, `None` when that
/// future was cancelled in flight. Collapsing the mixed set to one `Err` would
/// drop completed siblings and let cleanup emit a second terminal update for an
/// already-closed `tool_call_id`. Non-cancellation errors still abort.
pub(crate) async fn execute_tools_parallel(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
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
                tools_registry,
                activated_tools,
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

/// Cancellation contract: a cancel mid-batch stops dispatch and returns `Ok`
/// with one slot per call — `Some` for completed calls (a strict prefix), `None`
/// for the cut-short tail. Never an error. The token is checked before each call
/// so a tool that fires the token never lets a later call start, and a cancel
/// that interrupts a running tool leaves that call's slot `None`.
pub(crate) async fn execute_tools_sequential(
    tool_calls: &[ParsedToolCall],
    tools_registry: &[Box<dyn Tool>],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
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
            tools_registry,
            activated_tools,
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
    use super::execute_one_tool;
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

    /// Regression: execute_one_tool must recover a poisoned
    /// ActivatedToolSet mutex and still resolve the activated tool
    /// instead of panicking.
    ///
    /// Before the fix, the code used `.lock().unwrap()`, which panics
    /// on a poisoned mutex. The recovery path (`into_inner()`) allows
    /// the turn to proceed with the last valid state of the activated
    /// tool set.
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
            agent_alias: None,
            turn_id: "test-turn-id",
            channel_name: "test",
        };
        let outcome = execute_one_tool(
            "docker-mcp__extract_text",
            serde_json::json!({}),
            None,
            &[], // no static tools — force activated-tools path
            Some(&activated),
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

    // Pinned regression for the `tool_search` branch of
    // `should_execute_tools_in_parallel` (issue #7686, parent tracker #7685).
    //
    // `tool_search` activates deferred MCP tools into `ActivatedToolSet`. The
    // production comment on lines 345–348 explains why this branch exists:
    // running `tool_search` in parallel with the tools it activates can race
    // the lookup before activation completes. This branch forces serial
    // execution.
    //
    // PR #8040 covered the `tool_search` serial branch (3 tests below).
    // PR #8222 — rebased onto #8040 — adds the approval-required branch
    // (3 tests) and the parallel-when-allowed control (3 tests), sharing
    // these imports and the `parsed_tool_call` helper so the two PRs can
    // land in either order without E0252 duplicate-import collisions.
    //
    // Pre-existing tests in `loop_.rs` cover the single-call,
    // approval-required, and parallel control paths but leave the
    // `tool_search` branch untested. A future refactor that removes the
    // branch as "seems redundant because we hold a mutex" would silently
    // regress this contract — these tests pin it.
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

    // --- tool_search branch (#8040) ---

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
        // Control case (issue #7686 acceptance criterion #4): a batch that
        // contains neither `tool_search` nor any approval-gated tool must
        // remain parallel-eligible. This pins the default-true return so a
        // future refactor that turns the policy helper into a defensive
        // always-serial function is caught here, not at a much later
        // integration test. Issue #7686 only requires the inverse direction
        // (tool_search ⇒ serial); this test makes the "default still works"
        // half of the contract explicit in `tool_execution.rs` itself rather
        // than relying solely on the pre-existing control test in `loop_.rs`.
        let calls = vec![
            parsed_tool_call("file_read"),
            parsed_tool_call("memory_recall"),
        ];

        assert!(
            should_execute_tools_in_parallel(&calls, None),
            "non-tool_search, non-approval batch must remain parallel-eligible (default branch)"
        );
    }

    // --- approval-required + control branches (#8222) ---

    #[test]
    fn approval_required_batch_forces_sequential() {
        // A batch containing `shell` (always_ask in supervised) must stay
        // sequential so the caller can enforce the prompt/deny policy
        // uniformly. Without this, an approval-gated call could race with a
        // non-approval sibling and produce inconsistent state.
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
}
