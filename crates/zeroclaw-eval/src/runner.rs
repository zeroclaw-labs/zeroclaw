//! The runner: builds an isolated agent per case, drives it, and grades it.

use std::path::Path;
use std::sync::Arc;

use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_memory::{Memory, create_memory};
use zeroclaw_runtime::agent::agent::Agent;
use zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher;

use crate::Mode;
use crate::case::{LlmTrace, load_suite};
use crate::grader::evaluate_expects;
use crate::observer::RecordingObserver;
use crate::record::RunRecord;
use crate::replay::TraceLlmProvider;
use crate::report::{CaseReport, SuiteReport};
use crate::tools::default_tools;

/// Run every `*.json` trace fixture in `dir` and return an aggregated report.
pub async fn run_suite(dir: &Path, mode: Mode) -> anyhow::Result<SuiteReport> {
    if mode == Mode::Live {
        anyhow::bail!("live mode is not implemented yet (Phase 0 supports --mode replay only)");
    }

    let traces = load_suite(dir)?;
    if traces.is_empty() {
        anyhow::bail!("no *.json trace fixtures found in {}", dir.display());
    }

    let mut cases = Vec::with_capacity(traces.len());
    for (path, trace) in traces {
        let name = trace.model_name.clone();
        let source = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        let report = match run_case(&trace).await {
            Ok(record) => CaseReport {
                name,
                source,
                grades: evaluate_expects(&trace.expects, &record),
                error: None,
            },
            Err(e) => CaseReport {
                name,
                source,
                grades: vec![],
                error: Some(e.to_string()),
            },
        };
        cases.push(report);
    }

    Ok(SuiteReport { cases })
}

/// Replay a single trace through a freshly built, isolated agent and capture the run.
pub async fn run_case(trace: &LlmTrace) -> anyhow::Result<RunRecord> {
    // Each case gets an isolated temp workspace and an ephemeral "none" memory
    // backend so cases cannot observe one another.
    let tmp = tempfile::tempdir()?;

    let mem_cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    let memory: Arc<dyn Memory> = Arc::from(create_memory(&mem_cfg, tmp.path(), None)?);

    let observer = Arc::new(RecordingObserver::new());
    let replay = TraceLlmProvider::from_trace(trace);
    let replay_handle = replay.handle();
    let provider: Box<dyn ModelProvider> = Box::new(replay);

    // The engine's tool registry is sealed (`ScopedToolRegistry`), mintable only
    // through the one assembly seam. Route the eval harness's fixed tool set
    // through it with a permissive default policy so `assemble` is an identity
    // over `default_tools()` (nothing added, nothing dropped): the eval agent
    // sees exactly the same tools as before the seal. Every assembly divergence
    // is off (no peripherals / MCP / skills / memory-strip), so the config,
    // alias, and runtime are never read beyond satisfying the signature.
    let eval_config = zeroclaw_config::schema::Config::default();
    let eval_security = Arc::new(zeroclaw_config::policy::SecurityPolicy::default());
    let eval_registry = zeroclaw_runtime::tools::scoped::ScopedToolRegistry::assemble(
        zeroclaw_runtime::tools::scoped::ScopedAssembly {
            config: &eval_config,
            agent_alias: "eval",
            security: &eval_security,
            built: zeroclaw_runtime::tools::AllToolsResult::from_prebuilt_tools(default_tools()),
            skills: &[],
            runtime: Arc::new(zeroclaw_runtime::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        },
    )
    .await
    .registry;

    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(eval_registry)
        .memory(memory)
        .observer(observer.clone())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(tmp.path().to_path_buf())
        .build()?;

    let mut final_response = String::new();
    for (turn_index, turn) in trace.turns.iter().enumerate() {
        final_response = agent.turn(&turn.user_input).await?;
        // Enforce the turn boundary: every step scripted for this turn must have been
        // consumed before the next turn begins, so responses cannot bleed across turns.
        replay_handle.finish_turn(turn_index)?;
    }

    let (input_tokens, output_tokens) = observer.tokens();
    Ok(RunRecord {
        final_response,
        history: agent.history().to_vec(),
        tools_called: observer.tool_names(),
        all_tools_succeeded: observer.all_tools_succeeded(),
        input_tokens,
        output_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMOKE: &str = r#"{
        "model_name": "test-smoke-greeting",
        "turns": [{
            "user_input": "Hello, how are you?",
            "steps": [{ "response": { "type": "text", "content": "Hello! I'm doing well.", "input_tokens": 20, "output_tokens": 15 } }]
        }],
        "expects": { "response_contains": ["Hello"], "response_not_contains": ["error"], "max_tool_calls": 0 }
    }"#;

    const ECHO: &str = r#"{
        "model_name": "test-single-tool-echo",
        "turns": [{
            "user_input": "Echo hello for me",
            "steps": [
                { "response": { "type": "tool_calls", "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": {"message": "hello"} }], "input_tokens": 30, "output_tokens": 15 } },
                { "response": { "type": "text", "content": "The echo tool said: hello", "input_tokens": 50, "output_tokens": 10 } }
            ]
        }],
        "expects": { "response_contains": ["hello"], "tools_used": ["echo"], "max_tool_calls": 1, "all_tools_succeeded": true }
    }"#;

    #[tokio::test]
    async fn replays_text_only_trace() {
        let trace: LlmTrace = serde_json::from_str(SMOKE).unwrap();
        let record = run_case(&trace).await.unwrap();
        assert!(record.final_response.contains("Hello"));
        assert!(record.tools_called.is_empty());
        let grades = evaluate_expects(&trace.expects, &record);
        assert!(grades.iter().all(|g| g.passed), "grades: {grades:?}");
    }

    #[tokio::test]
    async fn replays_tool_call_trace() {
        let trace: LlmTrace = serde_json::from_str(ECHO).unwrap();
        let record = run_case(&trace).await.unwrap();
        assert_eq!(record.tools_called, vec!["echo".to_string()]);
        assert!(record.all_tools_succeeded);
        let grades = evaluate_expects(&trace.expects, &record);
        assert!(grades.iter().all(|g| g.passed), "grades: {grades:?}");
    }

    #[tokio::test]
    async fn live_mode_is_rejected_in_phase_0() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_suite(dir.path(), Mode::Live).await.unwrap_err();
        assert!(err.to_string().contains("live mode is not implemented"));
    }

    const MULTI_TURN: &str = r#"{
        "model_name": "test-multi-turn",
        "turns": [
            { "user_input": "Hi", "steps": [{ "response": { "type": "text", "content": "Hello there." } }] },
            { "user_input": "And goodbye?", "steps": [{ "response": { "type": "text", "content": "Goodbye!" } }] }
        ],
        "expects": {}
    }"#;

    #[tokio::test]
    async fn replays_multi_turn_trace_in_order() {
        let trace: LlmTrace = serde_json::from_str(MULTI_TURN).unwrap();
        let record = run_case(&trace).await.unwrap();
        // The final response comes from the *last* turn, proving turns replay in order
        // with each turn's step consumed within its own boundary.
        assert!(
            record.final_response.contains("Goodbye"),
            "final response: {:?}",
            record.final_response
        );
    }

    #[tokio::test]
    async fn over_specified_turn_is_an_error() {
        // The turn declares two steps but the agent makes a single chat() call, so the
        // extra step is left unconsumed. Under the old flat queue this passed silently;
        // now it must surface as a turn-scoped error rather than bleed into a next turn.
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "test-over-specified",
                "turns": [{ "user_input": "Hi", "steps": [
                    { "response": { "type": "text", "content": "Hello there." } },
                    { "response": { "type": "text", "content": "unused extra step" } }
                ] }],
                "expects": {}
            }"#,
        )
        .unwrap();
        let err = match run_case(&trace).await {
            Ok(_) => panic!("expected an error: an over-specified turn left a step unconsumed"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("over-specifies") || msg.contains("never requested"),
            "unexpected error: {msg}"
        );
    }
}
