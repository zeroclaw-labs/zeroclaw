//! Live execution mode: drive a case against a real provider inside a per-case
//! sandbox (temp workspace, `workspace_only` policy, allowlist-intersected tool
//! registry, deny-by-default approvals, per-turn timeout).

use std::path::Path;
use std::sync::Arc;

use zeroclaw_api::tool::Tool;
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{
    AliasedAgentConfig, MemoryConfig, RiskProfileConfig, SandboxBackend, SandboxConfig,
};
use zeroclaw_memory::{Memory, create_memory};
use zeroclaw_runtime::agent::agent::{Agent, tool_dispatcher_for_provider};
use zeroclaw_runtime::approval::ApprovalManager;
use zeroclaw_runtime::security::Sandbox;

use crate::case::{CaseSetup, LlmTrace, validate_workspace_rel_path};
use crate::observer::RecordingObserver;
use crate::record::RunRecord;
use crate::runner::{CaseProvider, RunDeps};

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
/// registry (a harmless deterministic tool). With an allowlist that includes
/// `shell`, the shell tool MUST be wrapped in a real OS sandbox backend before
/// it is allowed to run: this is fail-closed, not fail-open, so a platform with
/// no available sandbox backend errors here rather than silently falling back
/// to an unsandboxed shell. With an allowlist that doesn't include `shell`,
/// the runtime default tools (unsandboxed shell tool included, but filtered
/// out below) are used as-is, and the registry filter is the primary guard; the
/// builder allowlist (set by the caller) is defense in depth.
fn live_tool_registry(
    effective: &[String],
    policy: Arc<SecurityPolicy>,
) -> anyhow::Result<Vec<Box<dyn Tool>>> {
    if effective.is_empty() {
        return Ok(crate::tools::default_tools());
    }
    let mut tools = if effective.iter().any(|t| t == "shell") {
        let sandbox = live_shell_sandbox(&policy.workspace_dir)?;
        zeroclaw_runtime::tools::default_tools_with_sandbox(policy, sandbox)
    } else {
        zeroclaw_runtime::tools::default_tools(policy)
    };
    tools.retain(|t| effective.iter().any(|name| name == t.name()));
    Ok(tools)
}

/// Resolve the OS sandbox backend that will confine the live shell tool's
/// subprocesses to `workspace` (plus each backend's fixed system-temp
/// allowance; see the threat-model doc). Fails closed via
/// [`ensure_real_sandbox`] if no real backend is available on this platform.
pub fn live_shell_sandbox(workspace: &Path) -> anyhow::Result<Arc<dyn Sandbox>> {
    let sandbox = zeroclaw_runtime::security::create_sandbox(
        &SandboxConfig {
            enabled: Some(true),
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        },
        "native",
        Some(workspace),
    );
    ensure_real_sandbox(sandbox.as_ref())?;
    Ok(sandbox)
}

/// Fail-closed guard: a live case that requests `shell` must run under a real
/// OS sandbox backend. `create_sandbox` falls back to `NoopSandbox` (name
/// `"none"`) when no backend is available or detection fails; that fallback is
/// the right choice for the rest of the runtime (application-layer security
/// still applies), but is never acceptable for live mode's real-provider shell
/// access, so it is rejected here rather than silently accepted.
fn ensure_real_sandbox(sandbox: &dyn Sandbox) -> anyhow::Result<()> {
    if sandbox.name() == "none" {
        anyhow::bail!(
            "live case requests 'shell' but no OS sandbox backend is available on this platform \
             (Landlock [Linux, feature sandbox-landlock], Firejail, or sandbox-exec [macOS]); \
             refusing to run shell unsandboxed - remove 'shell' from case tools / \
             [eval].live_allowed_tools or enable a sandbox backend"
        );
    }
    Ok(())
}

/// Drive one live case: build a sandboxed agent, run each turn under a wall-clock
/// timeout, and capture the run for grading.
pub async fn run_live_case(trace: &LlmTrace, deps: &RunDeps) -> anyhow::Result<RunRecord> {
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

    let tools = live_tool_registry(&effective, policy.clone())?;
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
    // `finish_turn` is the replay-only per-turn exhaustion boundary (see
    // `runner::run_replay_case`); live must never call it. Driver traces used in
    // live tests script every step within one driver-side turn that is not
    // aligned to the case's turns, and a real live provider has no scripted
    // queue at all, so there is no per-turn boundary to enforce here.
    let CaseProvider {
        provider,
        provider_name,
        model_name,
        finish_turn: _,
    } = (deps.provider)(trace)?;
    // Resolve the dispatcher from the provider's capabilities so XML-dialect
    // providers work; a default agent config routes purely by capability.
    let dispatcher =
        tool_dispatcher_for_provider(&AliasedAgentConfig::default(), provider.as_ref());

    let mut builder = Agent::builder()
        .model_provider(provider)
        .tools(tools)
        .memory(memory)
        .observer(observer.clone())
        .tool_dispatcher(dispatcher)
        .workspace_dir(tmp.path().to_path_buf())
        .allowed_tools(allowed_arg)
        .autonomy_level(AutonomyLevel::Supervised)
        .approval_manager(Some(approvals));
    if let Some(model) = model_name {
        builder = builder.model_name(model);
    }
    if let Some(ptype) = provider_name {
        builder = builder.model_provider_name(ptype);
    }
    let mut agent = builder.build()?;

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
    /// The factory returns a bare boxed provider; it is wrapped in a
    /// metadata-free `CaseProvider` here so existing tests don't need to know
    /// about the `CaseProvider` boundary.
    fn live_deps(
        provider: impl Fn(&LlmTrace) -> anyhow::Result<Box<dyn ModelProvider>> + Send + Sync + 'static,
        live_tools: Vec<String>,
        timeout: Duration,
    ) -> RunDeps {
        RunDeps {
            mode: Mode::Live,
            provider: Box::new(move |trace| Ok(CaseProvider::from_provider(provider(trace)?))),
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
        let registry = live_tool_registry(&[], policy).unwrap();
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name(), "echo");
    }

    #[test]
    fn noop_sandbox_is_rejected_for_live_shell() {
        // The fail-closed guard: a `NoopSandbox` (the "no real backend available"
        // fallback `create_sandbox` returns) must never be accepted for a live
        // case that requests `shell`. The error must name the remediation
        // (drop `shell` from the allowlist, or enable a sandbox backend).
        let err = ensure_real_sandbox(&zeroclaw_runtime::security::NoopSandbox).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no OS sandbox backend is available"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("live_allowed_tools") || msg.contains("enable a sandbox backend"),
            "error must name a remediation: {msg}"
        );
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

        let record = run_live_case(&trace, &deps).await.unwrap();
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
            !record.all_tools_succeeded,
            "the out-of-workspace file_write must not report success"
        );
    }

    /// A provider that records the `model` string it is called with on every
    /// `chat` invocation, so tests can assert the agent was actually built with
    /// the configured model rather than `Agent::builder()`'s "<unconfigured>"
    /// default.
    struct ModelRecordingProvider {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl Attributable for ModelRecordingProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "recorder"
        }
    }
    #[async_trait]
    impl ModelProvider for ModelRecordingProvider {
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
            model: &str,
            _t: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            self.seen.lock().unwrap().push(model.to_string());
            Ok(ChatResponse {
                text: Some("ok".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn live_agent_calls_provider_with_configured_model() {
        // Before the `CaseProvider` metadata plumbing, `src/commands/eval.rs`
        // discarded the resolved provider type and model name returned by
        // `build_session_model_provider`, so the live agent was always built
        // with `Agent::builder()`'s "<unconfigured>" default model regardless
        // of what `[eval].live_provider` configured. This asserts every `chat`
        // call the agent makes carries the configured model name through.
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_for_factory = seen.clone();

        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "metadata-case", "turns": [{ "user_input": "hi" }] }"#,
        )
        .unwrap();

        let deps = RunDeps {
            mode: Mode::Live,
            provider: Box::new(move |_trace: &LlmTrace| {
                Ok(CaseProvider {
                    provider: Box::new(ModelRecordingProvider {
                        seen: seen_for_factory.clone(),
                    }),
                    provider_name: Some("testprov".to_string()),
                    model_name: Some("model-under-test".to_string()),
                    finish_turn: None,
                })
            }),
            live_tools: Vec::new(),
            case_timeout: Duration::from_secs(5),
        };

        run_live_case(&trace, &deps).await.unwrap();

        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "provider.chat was never called");
        assert!(
            seen.iter().all(|m| m == "model-under-test"),
            "every chat call must carry the configured model: {seen:?}"
        );
    }
}
