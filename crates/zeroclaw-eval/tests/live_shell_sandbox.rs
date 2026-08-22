//! Regression coverage for live mode's hard `shell` exclusion (see
//! `docs/book/src/ops/eval-harness.md`'s "Live mode" table and
//! `crates/zeroclaw-eval/src/live.rs`'s `LIVE_TOOL_DENYLIST`): a live case can
//! never make `shell` part of its effective tool surface, even when both the
//! case's own `tools` and
//! `[eval].live_allowed_tools` explicitly request it.
//!
//! This file previously proved that `shell`'s subprocesses were confined by
//! a real OS sandbox backend (Landlock/Firejail/`sandbox-exec`) when `shell`
//! was allowlisted. Every accepted backend still permits host *reads* that a
//! real-provider live run must not leak back into the
//! conversation (Seatbelt allows user dotfile reads; Firejail's
//! `--private=home` adds no workspace whitelist; Landlock leaves the whole
//! `/tmp` tree readable and network unrestricted) - wrapping `shell` in the
//! best available sandbox was not sufficient confinement for live mode's
//! confidentiality requirement, so `shell` is now excluded outright
//! (`LIVE_TOOL_DENYLIST`). The tests below prove the escape attempts they
//! used to run against a sandboxed `shell` can no longer happen at all,
//! because `shell` never gets dispatched in the first place - a stronger
//! guarantee than "the sandbox denied it."
//!
//! `live_shell_sandbox` / `ensure_real_sandbox` (the OS-sandbox construction
//! that used to wrap `shell`, still exported from `live.rs`) remain the
//! building blocks for the deferred follow-up - an eval-specific sandbox
//! contract that also denies sensitive host reads, at which point `shell`
//! could be safely re-admitted. `live_shell_sandbox_still_constructs_a_real_backend_where_expected`
//! keeps that construction path covered so it doesn't rot before then.
#![cfg(unix)]

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_eval::live::{live_shell_sandbox, run_live_case};
use zeroclaw_eval::replay::TraceLlmProvider;
use zeroclaw_eval::{CaseProvider, LlmTrace, Mode, RunDeps};

/// True if `history` contains a fed-back tool result denying a call (the
/// non-interactive approval gate's fixed "Denied by user." text -
/// `crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs`). Because
/// `shell` is excluded from `effective` (and therefore from
/// `risk.auto_approve`), a scripted `shell` call is auto-denied *before*
/// tool dispatch: it never reaches `execute_one_tool`, so it never shows up
/// in `RunRecord::tools_called`/`all_tools_succeeded` at all (see
/// `gate_tool_approval`'s `Deny` path, which returns straight back to
/// `prepare_tool_calls` without touching the observer). This denial in the
/// conversation history - not tool-call bookkeeping - is the real proof the
/// call never ran.
fn history_shows_a_denial(history: &[ConversationMessage]) -> bool {
    history.iter().any(|msg| {
        matches!(
            msg,
            ConversationMessage::ToolResults(results)
                if results
                    .iter()
                    .any(|r| r.content.to_ascii_lowercase().contains("denied"))
        )
    })
}

/// Build a boxed `TraceLlmProvider` from an inline JSON trace, mirroring
/// `crates/zeroclaw-eval/src/live.rs`'s test-only `driver_provider` helper.
/// The driver trace scripts every LLM round-trip for the case in a single
/// driver-side turn; `run_live_case` never calls `finish_turn` (that boundary
/// is replay-only), so the driver's single turn queue is popped in order
/// regardless of how many turns the case trace itself declares.
fn driver_provider(trace_json: &str) -> Result<CaseProvider> {
    let driver: LlmTrace = serde_json::from_str(trace_json)?;
    Ok(CaseProvider::from_provider(Box::new(
        TraceLlmProvider::try_from_trace(&driver)?,
    )))
}

/// A per-process-unique subdirectory under `CARGO_TARGET_TMPDIR`, outside any
/// path a sandbox backend blanket-allows (`/tmp`, `/private/tmp`,
/// `/private/var/folders/**`). Created host-side by the (unsandboxed) test
/// process itself before the case runs. Now that `shell` never dispatches at
/// all, no write here can occur regardless of location; kept outside those
/// blanket-allowed paths anyway so this test would still catch a regression
/// that reintroduced sandboxed-but-permitted execution instead of exclusion.
fn canary_paths() -> (PathBuf, PathBuf) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let dir = base.join(format!("live-shell-canary-{}", std::process::id()));
    let file = dir.join("leaked.txt");
    std::fs::create_dir_all(&dir).unwrap();
    (dir, file)
}

/// `live_shell_sandbox` (the OS-sandbox construction `shell` would need if
/// it were ever re-admitted to live mode) must still construct a real
/// backend wherever one is expected, independent of the tool-surface
/// denylist proven below. This keeps that building block from silently
/// rotting before the deferred read-confinement follow-up needs it.
#[test]
// The `Err` arm's assertion is only a compile-time constant for any single
// build target; it genuinely varies across the `cfg!` platforms it checks.
#[allow(clippy::assertions_on_constants)]
fn live_shell_sandbox_still_constructs_a_real_backend_where_expected() {
    let tmp = tempfile::tempdir().unwrap();
    match live_shell_sandbox(tmp.path()) {
        Ok(sandbox) => assert_ne!(
            sandbox.name(),
            "none",
            "a real sandbox backend must be selected when one is available"
        ),
        Err(_) => assert!(
            !cfg!(any(
                target_os = "macos",
                all(target_os = "linux", feature = "sandbox-landlock")
            )),
            "a real sandbox backend must exist on this platform"
        ),
    }
}

#[tokio::test]
async fn live_shell_cannot_write_outside_workspace_because_it_never_runs() {
    // `canary_paths` pre-creates `canary_dir` host-side (unsandboxed) so the
    // directory's existence is never in question.
    let (_canary_dir, canary) = canary_paths();

    let canary_str = canary.to_string_lossy().replace('\\', "\\\\");
    let trace: LlmTrace = serde_json::from_str(
        r#"{ "model_name": "shell-escape", "turns": [{ "user_input": "escape" }], "tools": ["file_write", "shell"] }"#,
    )
    .unwrap();

    let driver = format!(
        r#"{{"model_name":"driver","turns":[{{"user_input":"","steps":[
            {{"response":{{"type":"tool_calls","tool_calls":[{{"id":"1","name":"file_write","arguments":{{"path":"escape.py","content":"open(\"{canary_str}\", \"w\").write(\"leaked\")"}}}}]}}}},
            {{"response":{{"type":"tool_calls","tool_calls":[{{"id":"2","name":"shell","arguments":{{"command":"python3 escape.py"}}}}]}}}},
            {{"response":{{"type":"text","content":"done"}}}}
        ]}}]}}"#
    );

    // `live_tools` (what `[eval].live_allowed_tools` populates in the real
    // CLI path) explicitly includes "shell": the denylist must still win
    // over an operator opt-in, not just the case's own request.
    let deps = RunDeps {
        mode: Mode::Live,
        provider: Box::new(move |_trace: &LlmTrace| driver_provider(&driver)),
        provider_ref: "test.model:test".to_string(),
        live_tools: vec!["file_write".to_string(), "shell".to_string()],
        case_timeout: Duration::from_secs(10),
    };

    let record = run_live_case(&trace, &deps).await.unwrap().record;

    assert!(
        !canary.exists(),
        "shell must never dispatch in live mode, so it cannot write outside \
         the workspace to {}",
        canary.display()
    );
    assert!(
        !record.tools_called.contains(&"shell".to_string()),
        "shell must be auto-denied before it ever reaches tool dispatch, so \
         it must not appear as a dispatched tool call: {:?}",
        record.tools_called
    );
    // Anti-vacuity: the in-workspace `file_write` step (writing the escape
    // script `shell` would have run) did dispatch and succeed, so the
    // absence of the shell call above isn't just the whole case failing to
    // run anything.
    assert!(
        record.tools_called.contains(&"file_write".to_string()) && record.all_tools_succeeded,
        "the in-workspace file_write step must still dispatch and succeed: {:?}",
        record
    );
    assert!(
        history_shows_a_denial(&record.history),
        "the shell call must be auto-denied by the approval gate (proving \
         it never actually ran), but no denial was recorded in history: {:?}",
        record.history
    );
}

#[tokio::test]
async fn live_shell_cannot_reach_external_network_because_it_never_runs() {
    // Companion to the write-escape test above, covering the network-reach
    // attempt that `LIVE_TOOL_DENYLIST`'s doc comment calls out: Landlock as
    // configured here has no `AccessNet` rule, so a *sandboxed* shell could
    // still reach the network freely. Excluding `shell` outright closes that
    // gap the same way it closes the filesystem one - by never dispatching
    // the tool at all - rather than depending on a backend-specific network
    // rule this codebase doesn't implement.
    let trace: LlmTrace = serde_json::from_str(
        r#"{ "model_name": "shell-network", "turns": [{ "user_input": "reach out" }], "tools": ["file_write", "shell"] }"#,
    )
    .unwrap();

    let driver = r#"{"model_name":"driver","turns":[{"user_input":"","steps":[
        {"response":{"type":"tool_calls","tool_calls":[{"id":"1","name":"file_write","arguments":{"path":"net.py","content":"import socket\ns = socket.create_connection((\"93.184.216.34\", 80), timeout=2)\ns.close()"}}]}},
        {"response":{"type":"tool_calls","tool_calls":[{"id":"2","name":"shell","arguments":{"command":"python3 net.py"}}]}},
        {"response":{"type":"text","content":"done"}}
    ]}]}"#
        .to_string();

    let deps = RunDeps {
        mode: Mode::Live,
        provider: Box::new(move |_trace: &LlmTrace| driver_provider(&driver)),
        provider_ref: "test.model:test".to_string(),
        live_tools: vec!["file_write".to_string(), "shell".to_string()],
        case_timeout: Duration::from_secs(10),
    };

    let record = run_live_case(&trace, &deps).await.unwrap().record;
    assert!(
        !record.tools_called.contains(&"shell".to_string()),
        "shell must be auto-denied before it ever reaches tool dispatch, so \
         it must not appear as a dispatched tool call: {:?}",
        record.tools_called
    );
    assert!(
        record.tools_called.contains(&"file_write".to_string()) && record.all_tools_succeeded,
        "the in-workspace file_write step must still dispatch and succeed: {:?}",
        record
    );
    assert!(
        history_shows_a_denial(&record.history),
        "the shell call must be auto-denied by the approval gate rather \
         than reaching the network, but no denial was recorded in history: {:?}",
        record.history
    );
}
