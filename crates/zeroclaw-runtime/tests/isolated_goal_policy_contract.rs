//! Compare ordinary streamed and isolated Goal turns at the existing Agent seam.
//!
//! Run with `--features test-util` to construct the sealed fixture tool registry.

#![cfg(feature = "test-util")]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use zeroclaw_api::{
    attribution::{Attributable, ModelProviderKind, ProviderKind, Role, ToolKind},
    model_provider::{ChatRequest, ChatResponse, ToolCall},
    tool::{Tool, ToolResult},
};
use zeroclaw_goal::{GoalParentTurn, GoalParentTurnKind, control::goal_parent_turn_input};
use zeroclaw_runtime::agent::{
    agent::{Agent, IsolatedTranscriptSource},
    dispatcher::NativeToolDispatcher,
};

struct ScriptProvider {
    requests: Arc<Mutex<usize>>,
    tool_rounds: usize,
    calls_per_round: usize,
}

impl Attributable for ScriptProvider {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
    }

    fn alias(&self) -> &str {
        "isolated-policy-fixture"
    }
}

#[async_trait]
impl zeroclaw_api::model_provider::ModelProvider for ScriptProvider {
    fn supports_native_tools(&self) -> bool {
        true
    }

    async fn chat_with_system(
        &self,
        _system: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("fixture requires the tool request boundary")
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let mut requests = self.requests.lock().unwrap();
        *requests += 1;
        let round = *requests;
        Ok(ChatResponse {
            text: (round > self.tool_rounds).then(|| "policy candidate".to_owned()),
            tool_calls: if round <= self.tool_rounds {
                (0..self.calls_per_round)
                    .map(|slot| ToolCall {
                        id: format!("policy-{round}-{slot}"),
                        name: "echo".to_owned(),
                        arguments: "{}".to_owned(),
                        extra_content: None,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            usage: None,
            reasoning_content: None,
        })
    }
}

struct CountingTool(Arc<AtomicUsize>);

impl Attributable for CountingTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Plugin)
    }

    fn alias(&self) -> &str {
        "isolated-policy-fixture"
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Return the stable fixture result."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult {
            success: true,
            output: "stable tool result".into(),
            error: None,
        })
    }
}

async fn observe_policy(goal: bool, tool_rounds: usize, calls_per_round: usize) -> (usize, usize) {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(0));
    let mut agent = Agent::builder()
        .model_provider(Box::new(ScriptProvider {
            requests: requests.clone(),
            tool_rounds,
            calls_per_round,
        }))
        .tools(
            zeroclaw_runtime::tools::scoped::ScopedToolRegistry::from_raw_for_test(vec![Box::new(
                CountingTool(calls.clone()),
            )]),
        )
        .memory(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
        .observer(Arc::new(zeroclaw_runtime::observability::NoopObserver))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(tmp.path().to_owned())
        .build()
        .unwrap();
    if goal {
        let input = goal_parent_turn_input(&GoalParentTurn {
            kind: GoalParentTurnKind::Start,
            objective: "bounded repair".to_owned(),
            working_history: Vec::new(),
        });
        let result = agent
            .run_isolated_turn(
                IsolatedTranscriptSource::Canonical { prefix: Vec::new() },
                &input,
                "/goal start bounded repair",
            )
            .await
            .unwrap();
        assert_eq!(result.response, "policy candidate");
    } else {
        let (tx, mut rx) = tokio::sync::mpsc::channel(128);
        let drain = ::zeroclaw_spawn::spawn!(async move { while rx.recv().await.is_some() {} });
        let (result, _) = agent
            .turn_streamed("bounded repair", tx, None)
            .await
            .unwrap();
        assert_eq!(result, "policy candidate");
        drain.await.unwrap();
    }
    let request_count = *requests.lock().unwrap();
    (calls.load(Ordering::SeqCst), request_count)
}

#[tokio::test]
async fn isolated_goal_keeps_streamed_same_batch_calls_without_deduplication() {
    for goal in [false, true] {
        assert_eq!(
            observe_policy(goal, 1, 2).await,
            (2, 2),
            "native ZeroCode loop policy, goal={goal}"
        );
    }
}

#[tokio::test]
async fn isolated_goal_keeps_streamed_repeated_calls_without_a_detector_abort() {
    for goal in [false, true] {
        assert_eq!(
            observe_policy(goal, 6, 1).await,
            (6, 7),
            "native ZeroCode loop policy, goal={goal}"
        );
    }
}
