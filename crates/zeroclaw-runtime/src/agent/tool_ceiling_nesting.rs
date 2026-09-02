//! Nested-execution coverage for the turn capability ceiling.
//!
//! Lives beside `parity.rs` as a `#[path]` child of the agent module so it can
//! reach the same scripted-provider fixtures those tests use.

use std::sync::Arc;

/// `spawn_subagent` starts a whole agent run rather than a bare loop, so the
/// ceiling has to survive that entry point too. This drives the same
/// `Agent::turn_streamed_with_steering_state` the subagent path reaches
/// through `agent::run`, from inside a turn that removed the tool, and the
/// agent's own configuration excludes nothing.
#[tokio::test]
async fn an_agent_run_started_inside_a_turn_inherits_its_ceiling() {
    use super::safety_net::{
        CountingTool, ScriptedProvider, build_agent, tool_call, tool_response,
    };
    use crate::agent::tool_ceiling::with_tool_ceiling;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = ScriptedProvider::new(vec![tool_response(vec![tool_call("a", "echo")])]);
    let mut agent = build_agent(
        Box::new(provider),
        vec![Box::new(CountingTool {
            name: "echo",
            calls: Arc::clone(&calls),
        })],
    );
    let (tx, _rx) = mpsc::channel(256);

    let _ = with_tool_ceiling(
        &["echo".to_string()],
        Box::pin(agent.turn_streamed_with_steering_state("run", tx, None, None)),
    )
    .await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an agent run nested inside a restricted turn must not regain the removed tool"
    );
}

/// The invariant the nested-execution tools depend on, exercised through
/// the real loop twice rather than through the helper alone.
///
/// `NestingTool` stands in for `delegate`/`spawn_subagent`: it runs a child
/// `run_tool_call_loop` built the way they build theirs — from a policy
/// that excludes nothing, plus the ceiling resolved at call time. The child
/// provider then tries the tool the parent turn removed. Passing the
/// child's own (empty) exclusions instead, as those tools used to, makes
/// this fail with a count of 1.
#[tokio::test]
async fn a_child_loop_cannot_call_what_the_parent_turn_removed() {
    use super::safety_net::{ScriptedProvider, text_response, tool_call, tool_response};
    use crate::agent::loop_::{
        LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
        ToolLoop, run_tool_call_loop,
    };
    use crate::observability;
    use crate::tools::Tool;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::ingress::IngressContext;
    use zeroclaw_providers::ChatMessage;

    struct BlockedTool {
        calls: Arc<AtomicUsize>,
    }
    zeroclaw_api::tool_attribution!(BlockedTool, ::zeroclaw_api::attribution::ToolKind::Plugin);
    #[async_trait]
    impl Tool for BlockedTool {
        fn name(&self) -> &str {
            "blocked_tool"
        }
        fn description(&self) -> &str {
            "blocked_tool"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ran".into(),
                error: None,
            })
        }
    }

    struct NestingTool {
        blocked_calls: Arc<AtomicUsize>,
    }
    zeroclaw_api::tool_attribution!(NestingTool, ::zeroclaw_api::attribution::ToolKind::Plugin);
    #[async_trait]
    impl Tool for NestingTool {
        fn name(&self) -> &str {
            "nest"
        }
        fn description(&self) -> &str {
            "nest"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            // Exactly what `delegate` now passes: no static exclusions of
            // its own, plus the originating turn's ceiling.
            let inherited = crate::agent::tool_ceiling::current_tool_ceiling();
            let child_tools: Vec<Box<dyn Tool>> = vec![Box::new(BlockedTool {
                calls: Arc::clone(&self.blocked_calls),
            })];
            let child_provider = ScriptedProvider::new(vec![
                tool_response(vec![tool_call("c1", "blocked_tool")]),
                text_response("child done"),
            ]);
            let mut child_history = vec![ChatMessage::user("child")];
            let child_turn_id = "child-turn".to_string();
            let _ = Box::pin(run_tool_call_loop(ToolLoop {
                parent_agent_alias: None,
                sop_reassembly: None,
                exec: ResolvedAgentExecution::resolve(
                    ResolvedModelAccess {
                        model_provider: &child_provider,
                        provider_name: "mock",
                        model: "mock-model",
                        temperature: None,
                    },
                    ResolvedIo {
                        tools_registry: &child_tools,
                        observer: &observability::NoopObserver {},
                        silent: true,
                        approval: None,
                        multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                        config: None,
                        hooks: None,
                        activated_tools: None,
                        model_switch_callback: None,
                        receipt_generator: None,
                    },
                    ResolvedRuntimeKnobs {
                        max_tool_iterations: 5,
                        excluded_tools: &inherited,
                        dedup_exempt_tools: &[],
                        pacing: &zeroclaw_config::schema::PacingConfig::default(),
                        strict_tool_parsing: false,
                        parallel_tools: false,
                        max_tool_result_chars: 30_000,
                        context_token_budget: 100_000,
                        knobs: &LoopKnobs::default(),
                    },
                ),
                history: &mut child_history,
                channel_name: "delegate",
                channel_reply_target: None,
                cancellation_token: None,
                on_delta: None,
                shared_budget: None,
                channel: None,
                collected_receipts: None,
                event_tx: None,
                steering: None,
                new_messages_out: None,
                image_cache: None,
                ingress: IngressContext::sub_turn(),
                memory: None,
                agent_alias: None,
                turn_id: &child_turn_id,
            }))
            .await;

            Ok(crate::tools::ToolResult {
                success: true,
                output: "nested".into(),
                error: None,
            })
        }
    }

    let blocked_calls = Arc::new(AtomicUsize::new(0));
    let parent_tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NestingTool {
            blocked_calls: Arc::clone(&blocked_calls),
        }),
        Box::new(BlockedTool {
            calls: Arc::clone(&blocked_calls),
        }),
    ];
    let parent_provider = ScriptedProvider::new(vec![
        tool_response(vec![tool_call("t1", "nest")]),
        text_response("parent done"),
    ]);
    // The turn removed `blocked_tool` — the shape a skill's
    // `blocked_tools_with_image` produces on an image turn.
    let turn_exclusions = vec!["blocked_tool".to_string()];
    let mut history = vec![ChatMessage::user("hi")];
    let turn_id = "parent-turn".to_string();

    let _ = Box::pin(run_tool_call_loop(ToolLoop {
        parent_agent_alias: None,
        sop_reassembly: None,
        exec: ResolvedAgentExecution::resolve(
            ResolvedModelAccess {
                model_provider: &parent_provider,
                provider_name: "mock",
                model: "mock-model",
                temperature: None,
            },
            ResolvedIo {
                tools_registry: &parent_tools,
                observer: &observability::NoopObserver {},
                silent: true,
                approval: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                config: None,
                hooks: None,
                activated_tools: None,
                model_switch_callback: None,
                receipt_generator: None,
            },
            ResolvedRuntimeKnobs {
                max_tool_iterations: 5,
                excluded_tools: &turn_exclusions,
                dedup_exempt_tools: &[],
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: false,
                parallel_tools: false,
                max_tool_result_chars: 30_000,
                context_token_budget: 100_000,
                knobs: &LoopKnobs::default(),
            },
        ),
        history: &mut history,
        channel_name: "cli",
        channel_reply_target: None,
        cancellation_token: None,
        on_delta: None,
        shared_budget: None,
        channel: None,
        collected_receipts: None,
        event_tx: None,
        steering: None,
        new_messages_out: None,
        image_cache: None,
        ingress: IngressContext::sub_turn(),
        memory: None,
        agent_alias: None,
        turn_id: &turn_id,
    }))
    .await;

    assert_eq!(
        blocked_calls.load(Ordering::SeqCst),
        0,
        "a tool the turn removed must stay unexecuted through nested execution"
    );
}
