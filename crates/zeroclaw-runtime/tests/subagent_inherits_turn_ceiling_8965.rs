//! Integration test: a tool the turn's capability ceiling removed must stay
//! unreachable through `spawn_subagent`.
//!
//! The unit-level nesting coverage drives
//! `Agent::turn_streamed_with_steering_state`, which folds the inherited
//! ceiling into its own execution context. The real subagent path does not go
//! through it: `SpawnSubagentTool::execute` calls `crate::agent::run`, which
//! reaches `loop_::run`, and that entry point builds `excluded_tools` from
//! `compute_excluded_mcp_tools` alone. Publishing the ceiling into the
//! task-local while dispatching against that unmerged slice let a subagent
//! execute a tool the originating image turn had removed.
//!
//! Execution is counted by its side effect rather than by instrumenting the
//! registry: the scripted call is a `file_write` of a canary string to this
//! run's temp directory, so the canary file existing afterwards means the tool
//! executed. The unrestricted control run proves the call is otherwise
//! reachable, so the restricted assertion cannot pass vacuously.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Router, extract::State, routing::post};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
use zeroclaw_runtime::tools::SpawnSubagentTool;

/// Written only by the scripted tool call. Finding it on disk means the tool
/// the turn ceiling named actually executed.
const CANARY: &str = "ceiling-canary-8965-file-write-executed";

type CapturedBodies = Arc<AsyncMutex<Vec<String>>>;

/// Captured request bodies plus the absolute path the scripted tool call reads.
#[derive(Clone)]
struct MockState {
    captured: CapturedBodies,
    /// Absolute path inside this run's temp directory. Absolute because a
    /// relative path would resolve against the test process's working
    /// directory and write into the checkout.
    canary_path: String,
}

/// First response asks for the blocked tool; every later response ends the
/// turn, so a blocked run terminates instead of retrying forever.
async fn handle_chat(State(state): State<MockState>, body: String) -> String {
    let mut bodies = state.captured.lock().await;
    bodies.push(body);
    let first = bodies.len() == 1;
    drop(bodies);

    if first {
        json!({
            "id": "chatcmpl-ceiling",
            "object": "chat.completion",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "file_write",
                            "arguments": json!({
                                "path": state.canary_path,
                                "content": CANARY
                            })
                            .to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
        .to_string()
    } else {
        json!({
            "id": "chatcmpl-ceiling-done",
            "object": "chat.completion",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
        .to_string()
    }
}

async fn spawn_mock_provider(canary_path: String) -> (SocketAddr, CapturedBodies) {
    let captured: CapturedBodies = Arc::new(AsyncMutex::new(Vec::new()));
    let app = Router::new()
        .route("/chat/completions", post(handle_chat))
        .with_state(MockState {
            captured: captured.clone(),
            canary_path,
        });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    zeroclaw_spawn::spawn!(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });
    (addr, captured)
}

fn config_for(workspace_dir: &std::path::Path, provider_uri: &str) -> Config {
    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("custom", "default")
            .expect("`custom` slot must exist on ModelProviders");
        base.api_key = Some("test-key".to_string());
        base.model = Some("test-model".to_string());
        base.uri = Some(provider_uri.to_string());
        // Native tool calling: custom OpenAI-compatible endpoints default to
        // prompt-guided tools, and the scripted response below is a native
        // `tool_calls` payload.
        base.native_tools = Some(true);
    }

    let mut agent = AliasedAgentConfig {
        enabled: true,
        model_provider: "custom.default".into(),
        risk_profile: "default".into(),
        ..Default::default()
    };
    // Pin the workspace so the run stays inside this test's temp directory
    // rather than reaching an install-root path.
    agent.workspace.path = Some(workspace_dir.to_path_buf());
    let mut agents = HashMap::new();
    agents.insert("default".to_string(), agent);

    // Full autonomy with nothing statically excluded: the restriction under
    // test must come from the turn ceiling alone, not from policy that would
    // have blocked the call anyway.
    let mut risk_profiles = HashMap::new();
    risk_profiles.insert(
        "default".to_string(),
        RiskProfileConfig {
            level: AutonomyLevel::Full,
            require_approval_for_medium_risk: false,
            // The scripted write targets this run's temp directory, which the
            // default forbidden list rejects for being under /var. Nothing
            // here restricts the tool under test; the restriction being
            // measured must come from the turn ceiling alone.
            workspace_only: false,
            forbidden_paths: Vec::new(),
            allowed_roots: vec![workspace_dir.to_string_lossy().into_owned()],
            ..RiskProfileConfig::default()
        },
    );

    let mut config = Config {
        data_dir: workspace_dir.to_path_buf(),
        config_path: workspace_dir.join("config.toml"),
        providers,
        agents,
        risk_profiles,
        ..Config::default()
    };
    config.reliability.provider_retries = 0;
    config.reliability.scheduler_retries = 0;
    config
}

/// Run one `spawn_subagent` call, optionally inside a turn ceiling, and report
/// whether the scripted tool actually wrote its canary.
async fn canary_written_by_subagent(ceiling: &[String]) -> bool {
    let workspace = TempDir::new().unwrap();
    let canary_path = workspace.path().join("ceiling_canary.txt");
    let (addr, captured) = spawn_mock_provider(canary_path.to_string_lossy().into_owned()).await;
    let config = config_for(workspace.path(), &format!("http://{addr}"));

    // The parent policy is the ceiling the subagent's own policy is clamped to,
    // so it has to be the configured one rather than the default: a default
    // parent would block the scripted write for reasons unrelated to the turn
    // ceiling under test.
    let parent_policy = SecurityPolicy::from_risk_profile(
        config
            .risk_profiles
            .get("default")
            .expect("test risk profile"),
        workspace.path(),
    );
    let tool = SpawnSubagentTool::new(Arc::new(config), "default", Arc::new(parent_policy));
    let args = json!({ "prompt": "Read canary.txt and report what it says." });

    let call = tool.execute(args);
    if ceiling.is_empty() {
        let _ = Box::pin(call).await;
    } else {
        let _ =
            zeroclaw_runtime::agent::tool_ceiling::with_tool_ceiling(ceiling, Box::pin(call)).await;
    }

    // The subagent picks its own workspace under the run's data directory, so
    // search the tree rather than asserting on a path this test guessed.
    drop(captured);
    std::fs::read_to_string(&canary_path).is_ok_and(|body| body.contains(CANARY))
}

/// Control: with no ceiling the scripted call executes, so the restricted
/// assertion below is meaningful.
#[tokio::test]
async fn an_unrestricted_subagent_executes_the_scripted_tool() {
    assert!(
        canary_written_by_subagent(&[]).await,
        "control run must execute file_write, otherwise the restricted case proves nothing"
    );
}

/// A subagent spawned from a turn that removed `file_write` must not be able to
/// execute it. Fails with the canary present before the ceiling is folded into
/// the execution context the inner loop dispatches from.
#[tokio::test]
async fn a_subagent_cannot_execute_a_tool_the_turn_ceiling_removed() {
    assert!(
        !canary_written_by_subagent(&["file_write".to_string()]).await,
        "spawn_subagent regained a tool the originating turn's capability ceiling removed"
    );
}

/// The declaration a restriction comes from is hand-written, so the same entry
/// has to hold through the real dispatch path regardless of how it is spelled.
/// This is the direct-dispatch half of the shared-matcher coverage; the
/// wrapper and pipeline halves live beside their own enforcement points.
#[tokio::test]
async fn a_loosely_spelled_ceiling_entry_still_stops_the_subagent() {
    for spelling in ["  FiLe_WrItE  ", "FILE_WRITE", "\tfile_write\n"] {
        assert!(
            !canary_written_by_subagent(&[spelling.to_string()]).await,
            "{spelling:?} must remove the tool from the subagent's surface"
        );
    }
}
