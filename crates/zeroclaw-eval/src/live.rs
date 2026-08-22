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
use crate::record::{CaseProvenance, RunCompletion, RunRecord, ToolSurface};
use crate::runner::RunDeps;

/// Tools that may never be exposed to a live eval run, regardless of the case's
/// requested tools or the config allowlist.
///
/// `shell` spawns arbitrary child processes. `zeroclaw-runtime`'s `default_tools`
/// constructs `ShellTool::new`, whose default sandbox is `NoopSandbox`, so the
/// eval-side `workspace_only` policy and lexical path guards cannot confine what a
/// child process reads or which hosts it reaches — and a live case feeds tool
/// output straight back to a third-party model endpoint. The denial is
/// unconditional (not policy- or config-toggled) until an eval-specific OS sandbox
/// lands with read/write/network boundaries on every supported backend.
pub const LIVE_EVAL_DENIED_TOOLS: &[&str] = &["shell"];

/// Whether a tool name is hard-denied for live eval runs.
pub fn is_live_eval_denied(name: &str) -> bool {
    LIVE_EVAL_DENIED_TOOLS.contains(&name)
}

/// The tools a case asks for on this run: the case's requested tools intersected
/// with the config allowlist, verbatim — before the eval-side deny in
/// [`effective_live_tools`] and before registry filtering. Recorded as
/// [`ToolSurface::requested`] so the receipt shows what was asked for, including
/// names that were later denied or that no registry tool matches.
pub fn requested_live_tools(requested: Option<&[String]>, allowed: &[String]) -> Vec<String> {
    let requested = requested.unwrap_or(&[]);
    let mut out: Vec<String> = Vec::new();
    for tool in allowed {
        if requested.iter().any(|r| r == tool) && !out.iter().any(|o| o == tool) {
            out.push(tool.clone());
        }
    }
    out
}

/// Intersect a case's requested tools with the config allowlist, preserving the
/// allowlist's order and de-duplicating. An empty allowlist yields no tools.
///
/// Names in [`LIVE_EVAL_DENIED_TOOLS`] are removed unconditionally — the deny is
/// applied after the intersection, so it is not allowlist-shaped: a permissive
/// allowlist cannot re-admit them.
pub fn effective_live_tools(requested: Option<&[String]>, allowed: &[String]) -> Vec<String> {
    let mut out = requested_live_tools(requested, allowed);
    out.retain(|name| !is_live_eval_denied(name));
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
        // Defense in depth: the deny list is already applied in
        // `effective_live_tools`, but re-applying it here means no future caller
        // can construct a live registry containing a denied tool.
        tools.retain(|t| !is_live_eval_denied(t.name()));
        tools.retain(|t| effective.iter().any(|name| name == t.name()));
        tools
    }
}

/// Drive one live case: build a sandboxed agent, run each turn under a wall-clock
/// timeout, capture the run, and grade it while the workspace is still alive.
pub async fn run_live_case(
    trace: &LlmTrace,
    deps: &RunDeps,
    provenance_out: &mut Option<CaseProvenance>,
) -> anyhow::Result<crate::runner::CaseOutcome> {
    ensure_no_scripted_steps(trace)?;

    let requested = requested_live_tools(trace.tools.as_deref(), &deps.live_tools);
    let effective = effective_live_tools(trace.tools.as_deref(), &deps.live_tools);
    // Surface a hard-deny explicitly so a case author sees why their tool is
    // absent instead of silently getting a smaller surface than they asked for.
    for name in requested.iter().filter(|n| is_live_eval_denied(n)) {
        eprintln!(
            "live eval: tool '{name}' is hard-denied for live runs (no eval sandbox confines it) and was removed from case '{}'",
            trace.display_id()
        );
    }

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
    // Derive the recorded surface from the registry that is about to be handed to
    // `Agent::builder`, not from the request path. This is the only list that is
    // true in both directions: the empty-allowlist branch reports `["echo"]`
    // instead of `[]`, and an allowlisted name matching no runtime tool appears in
    // `requested` but not here.
    let registered: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    let provenance = crate::runner::case_provenance(
        trace,
        deps,
        ToolSurface::new(requested.clone(), effective.clone(), registered),
    )?;
    *provenance_out = Some(provenance.clone());

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
    // Live mode has no scripted steps, so no turn boundary to enforce.
    let crate::runner::CaseProvider { provider, .. } = (deps.provider)(trace)?;
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
    let record = RunRecord {
        provenance,
        completion: Some(RunCompletion {
            final_response,
            history: agent.history().to_vec(),
            tools_called: observer.tool_names(),
            all_tools_succeeded: observer.all_tools_succeeded(),
            input_tokens,
            output_tokens,
            duration_ms,
            llm_calls: observer.llm_calls(),
        }),
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
            provider: Box::new(move |trace| provider(trace).map(Into::into)),
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
        let requested = ["file_read".to_string(), "echo".to_string()];
        let allowed = ["echo".to_string()];
        assert_eq!(
            effective_live_tools(Some(&requested), &allowed),
            vec!["echo".to_string()]
        );
    }

    #[test]
    fn live_eval_denies_shell_even_when_allowlisted() {
        // Both the case and the config allowlist ask for `shell`; the eval-side
        // deny must remove it from the effective surface and from the registry.
        let requested = ["shell".to_string(), "file_read".to_string()];
        let allowed = ["shell".to_string(), "file_read".to_string()];
        let effective = effective_live_tools(Some(&requested), &allowed);
        assert!(
            !effective.iter().any(|t| t == "shell"),
            "shell must not survive admission: {effective:?}"
        );
        assert!(
            effective.iter().any(|t| t == "file_read"),
            "the deny must be surgical, not a blanket drop: {effective:?}"
        );

        let policy = Arc::new(SecurityPolicy::default());
        let registry = live_tool_registry(&effective, policy);
        assert!(
            !registry.iter().any(|t| t.name() == "shell"),
            "shell must be absent from the constructed live registry"
        );
    }

    #[test]
    fn live_eval_shell_denied_with_wildcard_allowlist() {
        // The deny must not be allowlist-shaped: even if a future caller hands the
        // registry builder a surface that names `shell`, the registry filter drops
        // it, and `requested_live_tools` -> `effective_live_tools` never keeps it.
        let allowed = ["shell".to_string()];
        let requested = ["shell".to_string()];
        assert!(requested_live_tools(Some(&requested), &allowed).contains(&"shell".to_string()));
        assert!(effective_live_tools(Some(&requested), &allowed).is_empty());

        let policy = Arc::new(SecurityPolicy::default());
        let registry = live_tool_registry(&["shell".to_string()], policy);
        assert!(
            !registry.iter().any(|t| t.name() == "shell"),
            "live_tool_registry must hard-deny shell regardless of the surface it is given"
        );
        assert!(is_live_eval_denied("shell"));
        assert!(!is_live_eval_denied("file_read"));
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
        let err = run_live_case(&trace, &deps, &mut None).await.unwrap_err();
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
        let err = run_live_case(&trace, &deps, &mut None).await.unwrap_err();
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

        let outcome = run_live_case(&trace, &deps, &mut None).await.unwrap();
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
            !outcome.record.completion_or_default().all_tools_succeeded,
            "the out-of-workspace file_write must not report success"
        );
    }

    #[tokio::test]
    async fn live_empty_tool_list_records_echo_in_registered_surface() {
        // No allowlisted tools -> the built-in echo registry is substituted. The
        // receipt must say `["echo"]`, not `[]`: reporting an empty surface hides
        // a capability the run genuinely had.
        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "echo-surface", "turns": [{ "user_input": "hi" }] }"#,
        )
        .unwrap();
        let deps = live_deps(
            |_| {
                Ok(driver_provider(
                    r#"{"model_name":"d","turns":[{"user_input":"","steps":[{"response":{"type":"text","content":"ok"}}]}]}"#,
                ))
            },
            Vec::new(),
            std::time::Duration::from_secs(5),
        );
        let outcome = run_live_case(&trace, &deps, &mut None).await.unwrap();
        let surface = &outcome.record.provenance.tool_surface;
        assert_eq!(
            surface.registered,
            vec!["echo".to_string()],
            "the implicit echo registry must be reported, not hidden behind []"
        );
        assert!(surface.effective.is_empty());
    }

    #[tokio::test]
    async fn live_unknown_allowlisted_tool_absent_from_registered_surface() {
        // A name present in both the case and the config allowlist but matching no
        // runtime tool must not be reported as available.
        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "unknown-tool", "turns": [{ "user_input": "hi" }], "tools": ["definitely_not_a_tool"] }"#,
        )
        .unwrap();
        let deps = live_deps(
            |_| {
                Ok(driver_provider(
                    r#"{"model_name":"d","turns":[{"user_input":"","steps":[{"response":{"type":"text","content":"ok"}}]}]}"#,
                ))
            },
            vec!["definitely_not_a_tool".to_string()],
            std::time::Duration::from_secs(5),
        );
        let outcome = run_live_case(&trace, &deps, &mut None).await.unwrap();
        let surface = &outcome.record.provenance.tool_surface;
        assert!(
            surface
                .requested
                .contains(&"definitely_not_a_tool".to_string()),
            "the request must be recorded verbatim: {surface:?}"
        );
        assert!(
            !surface
                .registered
                .contains(&"definitely_not_a_tool".to_string()),
            "a tool no registry exposes must not be reported as available: {surface:?}"
        );
    }

    #[tokio::test]
    async fn registered_surface_matches_agent_builder_registry() {
        // The recorded `registered` list must equal the names the registry hands to
        // `Agent::builder` — derived from the registry handle, not the request path.
        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "registry-match", "turns": [{ "user_input": "hi" }], "tools": ["file_read", "definitely_not_a_tool"] }"#,
        )
        .unwrap();
        let deps = live_deps(
            |_| {
                Ok(driver_provider(
                    r#"{"model_name":"d","turns":[{"user_input":"","steps":[{"response":{"type":"text","content":"ok"}}]}]}"#,
                ))
            },
            vec!["file_read".to_string(), "definitely_not_a_tool".to_string()],
            std::time::Duration::from_secs(5),
        );
        let outcome = run_live_case(&trace, &deps, &mut None).await.unwrap();
        let surface = &outcome.record.provenance.tool_surface;

        let effective = effective_live_tools(trace.tools.as_deref(), &deps.live_tools);
        let policy = Arc::new(SecurityPolicy::default());
        let mut expected: Vec<String> = live_tool_registry(&effective, policy)
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        expected.sort();
        assert_eq!(
            surface.registered, expected,
            "recorded surface must equal the registry actually built"
        );
        assert_eq!(surface.registered, vec!["file_read".to_string()]);
    }

    #[tokio::test]
    async fn timed_out_live_case_publishes_provenance_before_execution() {
        // A timeout aborts before any record is assembled; provenance must already
        // have been published so the caller can still build a receipt.
        let trace: LlmTrace = serde_json::from_str(
            r#"{ "model_name": "slow-prov", "turns": [{ "user_input": "hang" }] }"#,
        )
        .unwrap();
        let deps = live_deps(
            |_| Ok(Box::new(SleepProvider) as Box<dyn ModelProvider>),
            Vec::new(),
            std::time::Duration::from_millis(50),
        );
        let mut provenance = None;
        let err = run_live_case(&trace, &deps, &mut provenance)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "unexpected: {err}");
        let p = provenance.expect("provenance must be published before the turns loop");
        assert_eq!(p.case_id, "slow-prov");
        assert!(!p.case_hash.is_empty(), "case hash must survive a timeout");
        assert_eq!(p.tool_surface.registered, vec!["echo".to_string()]);
    }
}
