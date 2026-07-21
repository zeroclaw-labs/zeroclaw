//! Live execution mode: drive a case against a real provider inside a per-case
//! sandbox (temp workspace, `workspace_only` policy, allowlist-intersected tool
//! registry, deny-by-default approvals, per-turn timeout).

use std::path::Path;
use std::sync::Arc;

use zeroclaw_api::tool::Tool;
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{AliasedAgentConfig, MemoryConfig, RiskProfileConfig};
use zeroclaw_memory::{Memory, create_memory};
use zeroclaw_runtime::agent::agent::{Agent, tool_dispatcher_for_provider};
use zeroclaw_runtime::approval::ApprovalManager;

use crate::case::{CaseSetup, LlmTrace, validate_workspace_rel_path};
use crate::observer::RecordingObserver;
use crate::record::RunRecord;
use crate::runner::RunDeps;

/// Intersect a case's requested tools with the config allowlist, preserving the
/// allowlist's order and de-duplicating. An empty allowlist yields no tools.
pub fn effective_live_tools(requested: Option<&[String]>, allowed: &[String]) -> Vec<String> {
    let requested = requested.unwrap_or(&[]);
    let mut out: Vec<String> = Vec::new();
    for tool in allowed {
        if requested.iter().any(|r| r == tool) && !out.iter().any(|o| o == tool) {
            out.push(tool.clone());
        }
    }
    out
}

/// Reject live cases that script LLM steps: the real provider produces responses,
/// so scripted steps would be a contradiction (and silently ignored).
fn ensure_no_scripted_steps(trace: &LlmTrace) -> anyhow::Result<()> {
    for turn in &trace.turns {
        if turn.steps.as_deref().is_some_and(|s| !s.is_empty()) {
            anyhow::bail!("live case '{}' must not script LLM steps", trace.model_name);
        }
    }
    Ok(())
}

/// Write a case's setup files into `workspace`, validating every key as a safe
/// workspace-relative path first (so setup cannot escape the sandbox).
pub fn write_setup_files(workspace: &Path, setup: &CaseSetup) -> anyhow::Result<()> {
    for (rel, contents) in &setup.workspace_files {
        validate_workspace_rel_path(rel)?;
        let dest = workspace.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, contents)?;
    }
    Ok(())
}

/// Build the live tool registry. With no allowlisted tools, use the Phase 0 echo
/// registry (a harmless deterministic tool). With an allowlist, use the runtime
/// default tools filtered to the allowlist by name — the registry filter is the
/// primary guard; the builder allowlist (set by the caller) is defense in depth.
fn live_tool_registry(effective: &[String], policy: Arc<SecurityPolicy>) -> Vec<Box<dyn Tool>> {
    if effective.is_empty() {
        crate::tools::default_tools()
    } else {
        let mut tools = zeroclaw_runtime::tools::default_tools(policy);
        tools.retain(|t| effective.iter().any(|name| name == t.name()));
        tools
    }
}

/// Drive one live case: build a sandboxed agent, run each turn under a wall-clock
/// timeout, capture the run, and grade it while the workspace is still alive.
pub async fn run_live_case(
    trace: &LlmTrace,
    deps: &RunDeps,
) -> anyhow::Result<crate::runner::CaseOutcome> {
    ensure_no_scripted_steps(trace)?;

    let effective = effective_live_tools(trace.tools.as_deref(), &deps.live_tools);

    let tmp = tempfile::tempdir()?;
    if let Some(setup) = &trace.setup {
        write_setup_files(tmp.path(), setup)?;
    }

    let policy = Arc::new(SecurityPolicy {
        autonomy: AutonomyLevel::Supervised,
        workspace_dir: tmp.path().to_path_buf(),
        workspace_only: true,
        allowed_tools: Some(effective.clone()),
        ..SecurityPolicy::default()
    });

    // Deny-by-default approvals. Allowlisted tools are auto-approved (deterministic
    // pass-through); anything else that reaches the gate resolves Prompt -> auto-deny.
    // The backchannel variant closes the non-interactive shell-exemption hole.
    let risk = RiskProfileConfig {
        level: AutonomyLevel::Supervised,
        auto_approve: effective.clone(),
        always_ask: Vec::new(),
        ..RiskProfileConfig::default()
    };
    let approvals = Arc::new(ApprovalManager::for_non_interactive_backchannel(&risk));

    let tools = live_tool_registry(&effective, policy.clone());
    // Empty allowlist -> None so the echo registry's own tool is usable; a
    // `Some(vec![])` would deny every tool including echo. Non-empty -> the
    // allowlist backs the already-filtered registry as defense in depth.
    let allowed_arg = if effective.is_empty() {
        None
    } else {
        Some(effective.clone())
    };

    let mem_cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    let memory: Arc<dyn Memory> = Arc::from(create_memory(&mem_cfg, tmp.path(), None)?);

    let observer = Arc::new(RecordingObserver::new());
    let provider = (deps.provider)(trace)?;
    // Resolve the dispatcher from the provider's capabilities so XML-dialect
    // providers work; a default agent config routes purely by capability.
    let dispatcher =
        tool_dispatcher_for_provider(&AliasedAgentConfig::default(), provider.as_ref());

    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(tools)
        .memory(memory)
        .observer(observer.clone())
        .tool_dispatcher(dispatcher)
        .workspace_dir(tmp.path().to_path_buf())
        .allowed_tools(allowed_arg)
        .autonomy_level(AutonomyLevel::Supervised)
        .approval_manager(Some(approvals))
        .build()?;

    let start = std::time::Instant::now();
    let mut final_response = String::new();
    for (i, turn) in trace.turns.iter().enumerate() {
        match tokio::time::timeout(deps.case_timeout, agent.turn(&turn.user_input)).await {
            Ok(result) => final_response = result?,
            Err(_elapsed) => {
                anyhow::bail!(
                    "turn {} timed out after {}s",
                    i,
                    deps.case_timeout.as_secs()
                )
            }
        }
    }
    let duration_ms = start.elapsed().as_millis() as u64;

    let (input_tokens, output_tokens) = observer.tokens();
    let mut tool_surface = effective.clone();
    tool_surface.sort();
    let record = RunRecord {
        schema: crate::record::RECORD_SCHEMA.to_string(),
        mode: crate::Mode::Live,
        case_id: trace.display_id().to_string(),
        case_hash: crate::case::case_hash(trace)?,
        provider_ref: deps.provider_ref.clone(),
        tool_surface,
        sandbox: crate::record::SandboxStamp {
            autonomy: "supervised".to_string(),
            workspace_only: true,
        },
        final_response,
        history: agent.history().to_vec(),
        tools_called: observer.tool_names(),
        all_tools_succeeded: observer.all_tools_succeeded(),
        input_tokens,
        output_tokens,
        duration_ms,
        llm_calls: observer.llm_calls(),
    };
    // Grade while the temp workspace is still alive, then let `tmp` drop.
    let grades = crate::grader::grade_run(trace, &record, tmp.path()).await;
    Ok(crate::runner::CaseOutcome { record, grades })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;
    use crate::replay::TraceLlmProvider;
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{
        ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities,
    };

    /// Build a `RunDeps` for the live path with an injected provider factory.
    fn live_deps(
        provider: impl Fn(&LlmTrace) -> anyhow::Result<Box<dyn ModelProvider>> + Send + Sync + 'static,
        live_tools: Vec<String>,
        timeout: Duration,
    ) -> RunDeps {
        RunDeps {
            mode: Mode::Live,
            provider: Box::new(provider),
            provider_ref: "test.model:test".to_string(),
            live_tools,
            case_timeout: timeout,
        }
    }

    fn driver_provider(trace_json: &str) -> Box<dyn ModelProvider> {
        let driver: LlmTrace = serde_json::from_str(trace_json).unwrap();
        Box::new(TraceLlmProvider::try_from_trace(&driver).unwrap())
    }

    #[test]
    fn live_effective_tools_is_intersection() {
        let requested = ["shell".to_string(), "echo".to_string()];
        let allowed = ["echo".to_string()];
        assert_eq!(
            effective_live_tools(Some(&requested), &allowed),
            vec!["echo".to_string()]
        );
    }

    #[test]
    fn empty_allowlist_yields_echo_only_registry() {
        let policy = Arc::new(SecurityPolicy::default());
        let registry = live_tool_registry(&[], policy);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name(), "echo");
    }

    #[test]
    fn workspace_setup_rejects_absolute_and_parent_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let mut abs = BTreeMap::new();
        abs.insert("/etc/passwd".to_string(), "x".to_string());
        assert!(
            write_setup_files(
                tmp.path(),
                &CaseSetup {
                    workspace_files: abs
                }
            )
            .is_err()
        );

        let mut parent = BTreeMap::new();
        parent.insert("../escape.txt".to_string(), "x".to_string());
        assert!(
            write_setup_files(
                tmp.path(),
                &CaseSetup {
                    workspace_files: parent
                }
            )
            .is_err()
        );
    }

    #[test]
    fn workspace_setup_writes_nested_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = BTreeMap::new();
        files.insert("sub/dir/file.txt".to_string(), "hello".to_string());
        write_setup_files(
            tmp.path(),
            &CaseSetup {
                workspace_files: files,
            },
        )
        .unwrap();
        let written = std::fs::read_to_string(tmp.path().join("sub/dir/file.txt")).unwrap();
        assert_eq!(written, "hello");
    }

    #[tokio::test]
    async fn live_case_with_scripted_steps_errors() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{
                "model_name": "bad-live",
                "turns": [{ "user_input": "hi", "steps": [
                    { "response": { "type": "text", "content": "scripted" } }
                ] }]
            }"#,
        )
        .unwrap();
        let deps = live_deps(
            |_| {
                Ok(driver_provider(
                    r#"{"model_name":"d","turns":[{"user_input":"","steps":[{"response":{"type":"text","content":"x"}}]}]}"#,
                ))
            },
            Vec::new(),
            Duration::from_secs(5),
        );
        let err = run_live_case(&trace, &deps).await.unwrap_err();
        assert!(
            err.to_string().contains("must not script LLM steps"),
            "unexpected error: {err}"
        );
    }

    /// A provider whose `chat` sleeps longer than any reasonable test timeout.
    struct SleepProvider;
    impl Attributable for SleepProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "sleep"
        }
    }
    #[async_trait]
    impl ModelProvider for SleepProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                ..ProviderCapabilities::default()
            }
        }
        async fn chat_with_system(
            &self,
            _s: Option<&str>,
            _m: &str,
            _model: &str,
            _t: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
        async fn chat(
            &self,
            _r: ChatRequest<'_>,
            _model: &str,
            _t: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(ChatResponse {
                text: Some("late".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn live_turn_timeout_fails_case() {
        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "slow", "turns": [{ "user_input": "hang" }] }"#,
        )
        .unwrap();
        let deps = live_deps(
            |_| Ok(Box::new(SleepProvider) as Box<dyn ModelProvider>),
            Vec::new(),
            Duration::from_millis(50),
        );
        let err = run_live_case(&trace, &deps).await.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn live_sandbox_blocks_file_write_outside_workspace() {
        // A canary path outside the case workspace whose parent does NOT yet exist.
        // The scripted provider drives the agent to call file_write with this
        // absolute path; the workspace_only policy must block the write before any
        // filesystem side effect, so neither the file nor its parent dir appears.
        let canary_dir = tempfile::tempdir().unwrap();
        let canary_parent = canary_dir.path().join("newdir");
        let canary = canary_parent.join("leaked.txt");
        let canary_str = canary.to_string_lossy().replace('\\', "\\\\");

        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "escape", "turns": [{ "user_input": "write outside" }], "tools": ["file_write"] }"#,
        )
        .unwrap();

        let driver = format!(
            r#"{{"model_name":"driver","turns":[{{"user_input":"","steps":[
                {{"response":{{"type":"tool_calls","tool_calls":[{{"id":"1","name":"file_write","arguments":{{"path":"{canary_str}","content":"leak"}}}}]}}}},
                {{"response":{{"type":"text","content":"done"}}}}
            ]}}]}}"#
        );
        let deps = live_deps(
            move |_| Ok(driver_provider(&driver)),
            vec!["file_write".to_string()],
            Duration::from_secs(5),
        );

        let outcome = run_live_case(&trace, &deps).await.unwrap();
        assert!(
            !canary.exists(),
            "sandbox breach: file_write wrote outside the workspace to {}",
            canary.display()
        );
        // The guard must reject the path before any filesystem side effect: the
        // out-of-workspace parent directory must not be created either.
        assert!(
            !canary_parent.exists(),
            "sandbox breach: file_write created a directory outside the workspace at {}",
            canary_parent.display()
        );
        assert!(
            !outcome.record.all_tools_succeeded,
            "the out-of-workspace file_write must not report success"
        );
    }

    #[tokio::test]
    async fn repeated_runs_are_isolated() {
        // Run 1 writes marker.txt into its temp workspace; run 2 asserts the file
        // is absent. A fresh workspace per run means run 2 cannot see run 1's file.
        let write_case: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "iso-write", "turns": [{ "user_input": "write" }],
                 "tools": ["file_write"],
                 "expects": { "workspace": { "file_exists": ["marker.txt"] } } }"#,
        )
        .unwrap();
        let write_deps = live_deps(
            |_| {
                Ok(driver_provider(
                    r#"{"model_name":"d","turns":[{"user_input":"","steps":[
                {"response":{"type":"tool_calls","tool_calls":[{"id":"1","name":"file_write","arguments":{"path":"marker.txt","content":"hi"}}]}},
                {"response":{"type":"text","content":"done"}}
            ]}]}"#,
                ))
            },
            vec!["file_write".to_string()],
            Duration::from_secs(5),
        );
        let out1 = run_live_case(&write_case, &write_deps).await.unwrap();
        assert!(
            out1.grades.iter().all(|g| g.passed),
            "run 1 must write marker.txt into its own workspace: {:?}",
            out1.grades
        );

        // Run 2: a fresh case that does nothing and asserts marker.txt is absent.
        let absent_case: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "iso-absent", "turns": [{ "user_input": "noop" }],
                 "expects": { "workspace": { "file_absent": ["marker.txt"] } } }"#,
        )
        .unwrap();
        let noop_deps = live_deps(
            |_| {
                Ok(driver_provider(
                    r#"{"model_name":"d","turns":[{"user_input":"","steps":[{"response":{"type":"text","content":"noop"}}]}]}"#,
                ))
            },
            Vec::new(),
            Duration::from_secs(5),
        );
        let out2 = run_live_case(&absent_case, &noop_deps).await.unwrap();
        assert!(
            out2.grades.iter().all(|g| g.passed),
            "run 2's fresh workspace must not contain run 1's marker.txt: {:?}",
            out2.grades
        );
    }
}
