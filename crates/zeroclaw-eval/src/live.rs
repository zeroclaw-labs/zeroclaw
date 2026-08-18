//! Live execution mode: drive a case against a real provider inside a per-case
//! sandbox (temp workspace, `workspace_only` policy, allowlist-intersected tool
//! registry, deny-by-default approvals, per-turn timeout).

use std::path::Path;
use std::sync::Arc;

use zeroclaw_api::tool::Tool;
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{AliasedAgentConfig, MemoryConfig, RiskProfileConfig, RuntimeKind};
use zeroclaw_memory::{Memory, create_memory};
use zeroclaw_runtime::agent::agent::{Agent, tool_dispatcher_for_provider};
use zeroclaw_runtime::approval::ApprovalManager;
use zeroclaw_runtime::platform::{NativeRuntime, RuntimeAdapter};
use zeroclaw_runtime::security::{Sandbox, create_sandbox};
use zeroclaw_runtime::tools::{PathGuardedTool, RateLimitedTool, ShellTool};

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
/// registry (a harmless deterministic tool). With an allowlist, build the runtime
/// tools against an **enforcing** OS sandbox and filter them to the allowlist by
/// name.
///
/// `zeroclaw_runtime::tools::default_tools` constructs `ShellTool::new`, which
/// hardcodes a pass-through sandbox. Argument-level guards (`workspace_only`,
/// `PathGuardedTool`) only inspect visible tool arguments, so they cannot see
/// what an in-workspace script does once it is spawned. Live mode therefore
/// builds the shell tool with `ShellTool::new_with_sandbox` and fails closed
/// when no enforcing backend is available on this platform.
fn live_tool_registry(
    effective: &[String],
    policy: Arc<SecurityPolicy>,
) -> anyhow::Result<Vec<Box<dyn Tool>>> {
    live_tool_registry_with_sandbox(effective, policy, |workspace_dir| {
        create_sandbox(
            &RiskProfileConfig::default().sandbox_config(),
            RuntimeKind::default().as_wire(),
            Some(workspace_dir),
        )
    })
}

/// Testable core of [`live_tool_registry`], parameterised over sandbox creation
/// so a non-enforcing backend can be injected without touching global state.
fn live_tool_registry_with_sandbox(
    effective: &[String],
    policy: Arc<SecurityPolicy>,
    make_sandbox: impl FnOnce(&Path) -> Arc<dyn Sandbox>,
) -> anyhow::Result<Vec<Box<dyn Tool>>> {
    if effective.is_empty() {
        return Ok(crate::tools::default_tools());
    }

    let sandbox = make_sandbox(&policy.workspace_dir);
    if !sandbox.is_enforcing() {
        anyhow::bail!(
            "live eval requires an enforcing sandbox backend for its tool registry; \
             backend `{}` provides no OS-level filesystem confinement on this platform — \
             rerun with an empty `live_allowed_tools`, or configure a sandbox backend",
            sandbox.name()
        );
    }

    let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
    let shell = ShellTool::new_with_sandbox(policy.clone(), runtime, sandbox);
    let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(RateLimitedTool::new(
        PathGuardedTool::new(shell, policy.clone()),
        policy.clone(),
    ))];
    // Non-shell tools carry no OS-spawn surface; take them from the default
    // registry so the allowlist keeps working for `file_read`, `file_write`, etc.
    tools.extend(
        zeroclaw_runtime::tools::default_tools(policy)
            .into_iter()
            .filter(|t| t.name() != "shell"),
    );
    tools.retain(|t| effective.iter().any(|name| name == t.name()));

    // A name in the allowlist that no sandboxed tool provides is an error, not a
    // silent omission — otherwise a typo quietly downgrades the case.
    for name in effective {
        if !tools.iter().any(|t| t.name() == name) {
            anyhow::bail!(
                "live eval tool `{name}` is not available in the sandboxed live registry"
            );
        }
    }

    Ok(tools)
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
        judge_ref: crate::runner::judge_ref_for(trace, deps),
        judge_usage: None,
    };
    // Grade while the temp workspace is still alive, then let `tmp` drop.
    let grades = crate::grader::grade_run(trace, &record, tmp.path(), deps.judge.as_ref()).await;
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
            judge: None,
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
        let registry = live_tool_registry(&[], policy).expect("echo registry builds");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name(), "echo");
    }

    /// A backend that is "available" (it can always be constructed) but provides
    /// no OS-level confinement — exactly the shape of the pass-through sandbox
    /// the production path used to install silently.
    #[derive(Debug, Default)]
    struct NonEnforcingSandbox;

    impl Sandbox for NonEnforcingSandbox {
        fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn is_enforcing(&self) -> bool {
            false
        }

        fn name(&self) -> &str {
            "test-noop"
        }

        fn description(&self) -> &str {
            "test double with no confinement"
        }
    }

    fn live_policy(workspace: &Path, tools: &[String]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.to_path_buf(),
            workspace_only: true,
            allowed_tools: Some(tools.to_vec()),
            ..SecurityPolicy::default()
        })
    }

    /// The production registry must never hand back a shell tool over a
    /// pass-through sandbox. Asserted through the real `live_tool_registry`, not
    /// a hand-assembled registry.
    #[test]
    fn live_shell_tool_is_sandbox_enforcing() {
        let tmp = tempfile::tempdir().unwrap();
        let effective = vec!["shell".to_string()];
        let policy = live_policy(tmp.path(), &effective);

        let observed: std::cell::Cell<Option<bool>> = std::cell::Cell::new(None);
        let registry = live_tool_registry_with_sandbox(&effective, policy, |workspace_dir| {
            let sandbox = create_sandbox(
                &RiskProfileConfig::default().sandbox_config(),
                RuntimeKind::default().as_wire(),
                Some(workspace_dir),
            );
            observed.set(Some(sandbox.is_enforcing()));
            sandbox
        });

        match observed.get() {
            // Platform has a real backend: the registry must build over it, and
            // the same registry must still refuse a non-enforcing one — that
            // second half is what proves the shell is not taken from the
            // pass-through `default_tools` path regardless of the sandbox.
            Some(true) => {
                let registry = registry.expect("enforcing backend yields a registry");
                assert!(registry.iter().any(|t| t.name() == "shell"));

                let tmp2 = tempfile::tempdir().unwrap();
                let policy2 = live_policy(tmp2.path(), &effective);
                let refused = live_tool_registry_with_sandbox(&effective, policy2, |_| {
                    Arc::new(NonEnforcingSandbox) as Arc<dyn Sandbox>
                });
                assert!(
                    refused.is_err(),
                    "shell was built without consulting the sandbox backend"
                );
            }
            // Platform has none: the registry must refuse rather than quietly
            // hand back an unconfined shell.
            Some(false) => {
                let err = match registry {
                    Ok(_) => panic!("no enforcing backend must fail closed"),
                    Err(e) => e,
                };
                assert!(
                    err.to_string().contains("enforcing sandbox"),
                    "unexpected error: {err}"
                );
            }
            None => panic!("sandbox factory was never invoked for a non-empty allowlist"),
        }
    }

    /// Fail closed: when the platform offers only a pass-through backend, the
    /// registry must return an error naming the sandbox requirement.
    #[test]
    fn live_tool_registry_fails_closed_without_enforcing_sandbox() {
        let tmp = tempfile::tempdir().unwrap();
        let effective = vec!["shell".to_string()];
        let policy = live_policy(tmp.path(), &effective);

        let err = match live_tool_registry_with_sandbox(&effective, policy, |_| {
            Arc::new(NonEnforcingSandbox) as Arc<dyn Sandbox>
        }) {
            Ok(_) => panic!("non-enforcing backend must fail closed"),
            Err(e) => e,
        };

        let msg = err.to_string();
        assert!(msg.contains("enforcing sandbox"), "unexpected error: {msg}");
        assert!(
            msg.contains("test-noop"),
            "error should name the backend: {msg}"
        );
    }

    /// The pass-through sandbox must not claim confinement — this is the
    /// predicate the fail-closed check rests on.
    #[test]
    fn noop_sandbox_is_not_enforcing() {
        use zeroclaw_runtime::security::NoopSandbox;
        assert!(!NoopSandbox.is_enforcing());
        assert!(NoopSandbox.is_available());
    }

    /// An allowlisted name with no sandboxed tool behind it must be an error,
    /// not a silently smaller registry.
    #[test]
    fn live_tool_registry_rejects_unknown_allowlisted_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let effective = vec!["definitely_not_a_tool".to_string()];
        let policy = live_policy(tmp.path(), &effective);

        let err = match live_tool_registry_with_sandbox(&effective, policy, |_| {
            Arc::new(AlwaysEnforcingSandbox) as Arc<dyn Sandbox>
        }) {
            Ok(_) => panic!("unknown tool name must be rejected"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("definitely_not_a_tool"),
            "unexpected error: {err}"
        );
    }

    /// A test double that reports confinement, so registry-shape assertions do
    /// not depend on which backend the host platform happens to provide.
    #[derive(Debug, Default)]
    struct AlwaysEnforcingSandbox;

    impl Sandbox for AlwaysEnforcingSandbox {
        fn wrap_command(&self, _cmd: &mut std::process::Command) -> std::io::Result<()> {
            Ok(())
        }

        fn is_available(&self) -> bool {
            true
        }

        fn is_enforcing(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "test-enforcing"
        }

        fn description(&self) -> &str {
            "test double reporting confinement"
        }
    }

    /// A directory outside every path the sandbox policy grants writes to, used
    /// as the escape target. Temp roots are deliberately writable inside the
    /// sandbox, so probing one would pass vacuously. Returns `None` when no such
    /// location can be probed on this host.
    fn denied_outside_dir() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
        // Only usable if the harness itself can write here; otherwise a refused
        // write proves nothing about the sandbox.
        let probe = home.join(format!(".zc-eval-sandbox-probe-{}", std::process::id()));
        match std::fs::write(&probe, b"probe") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                Some(home)
            }
            Err(_) => None,
        }
    }

    /// Remove the escape target if a regression ever lets it be created, so a
    /// failing run does not litter the operator's home directory.
    struct RemoveOnDrop(std::path::PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Quote a path as a Python string literal so a Windows-style backslash or a
    /// quote in the temp path cannot break the generated script.
    fn python_string_literal(value: &str) -> String {
        let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
        format!("'{escaped}'")
    }

    /// Build the live shell tool exactly as production does, or report why no
    /// enforcing backend is available on this host.
    fn production_live_shell() -> Result<(tempfile::TempDir, Box<dyn Tool>), String> {
        let tmp = tempfile::tempdir().unwrap();
        let effective = vec!["shell".to_string()];
        let policy = live_policy(tmp.path(), &effective);
        match live_tool_registry(&effective, policy) {
            Ok(mut registry) => {
                let idx = registry
                    .iter()
                    .position(|t| t.name() == "shell")
                    .expect("shell present in a shell-only allowlist");
                Ok((tmp, registry.remove(idx)))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The escape Bot described: a script *inside* the workspace writing to a
    /// path outside it. The payload lives in the script body, so no
    /// argument-level guard can see it — only OS confinement stops this.
    #[tokio::test]
    async fn live_shell_in_workspace_script_cannot_write_outside_workspace() {
        let (tmp, shell) = match production_live_shell() {
            Ok(v) => v,
            Err(e) => {
                // Fail-closed path is asserted by
                // `live_tool_registry_fails_closed_without_enforcing_sandbox`.
                assert!(e.contains("enforcing sandbox"), "unexpected error: {e}");
                return;
            }
        };

        let Some(outside) = denied_outside_dir() else {
            // No location outside the sandbox's writable set is available to
            // probe on this host; the fail-closed and enforcement assertions in
            // the sibling tests still hold.
            return;
        };
        let target = outside.join(format!("zc-eval-escape-{}.txt", std::process::id()));
        let _cleanup = RemoveOnDrop(target.clone());
        // `python3` is on the default command allowlist, so the argument-level
        // guards admit this call. The escaping write lives inside the script
        // body, where no argument inspection can reach it — only OS-level
        // confinement can stop it.
        let script = tmp.path().join("escape.py");
        std::fs::write(
            &script,
            format!(
                "open({}, 'w').write('pwned')\n",
                python_string_literal(&target.to_string_lossy())
            ),
        )
        .unwrap();

        let result = shell
            .execute(serde_json::json!({ "command": "python3 escape.py" }))
            .await
            .expect("tool call returns a result");

        assert!(
            !result
                .error
                .as_deref()
                .is_some_and(|e| e.contains("not allowed by security policy")),
            "argument-level policy refused the call, so this test would pass \
             vacuously without exercising the sandbox: {result:?}"
        );
        assert!(
            !target.exists(),
            "in-workspace script escaped the sandbox and wrote {}",
            target.display()
        );
        assert!(
            !result.success,
            "escaping write should not report success: {result:?}"
        );
    }

    /// The symlink variant: an in-workspace name pointing out of the workspace.
    /// The visible argument is workspace-relative, so again only OS confinement
    /// can refuse it.
    #[tokio::test]
    async fn live_shell_does_not_follow_in_workspace_symlink_out() {
        let (tmp, shell) = match production_live_shell() {
            Ok(v) => v,
            Err(e) => {
                assert!(e.contains("enforcing sandbox"), "unexpected error: {e}");
                return;
            }
        };

        let Some(outside) = denied_outside_dir() else {
            return;
        };
        let link = tmp.path().join("out");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(not(unix))]
        {
            let _ = &link;
            return;
        }

        let target = outside.join(format!("zc-eval-symlink-{}.txt", std::process::id()));
        let _cleanup = RemoveOnDrop(target.clone());
        let script = tmp.path().join("via_symlink.py");
        std::fs::write(
            &script,
            format!(
                "open('out/{}', 'w').write('pwned')\n",
                target.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();

        let result = shell
            .execute(serde_json::json!({ "command": "python3 via_symlink.py" }))
            .await
            .expect("tool call returns a result");

        assert!(
            !result
                .error
                .as_deref()
                .is_some_and(|e| e.contains("not allowed by security policy")),
            "argument-level policy refused the call, so this test would pass \
             vacuously without exercising the sandbox: {result:?}"
        );

        assert!(
            !target.exists(),
            "write followed an in-workspace symlink out to {}",
            target.display()
        );
        assert!(
            !result.success,
            "symlink-escaping write should not report success: {result:?}"
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
