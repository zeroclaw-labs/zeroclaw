//! Regression coverage for the live-mode shell sandbox (see
//! `docs/book/src/ops/eval-harness.md`'s "Live mode" threat-model table): a
//! live case that requests the `shell` tool must have its subprocesses
//! confined by a real OS sandbox backend, never run unsandboxed.
//!
//! This is an integration test (rather than a unit test in `src/live.rs`) so
//! `env!("CARGO_TARGET_TMPDIR")` is available: the escape-attempt canary must
//! live outside `/tmp` and `/private/var/folders`, both of which the macOS
//! Seatbelt policy (and, on Linux, the fixed `/tmp` allowance baked into
//! `LandlockSandbox::build_ruleset`) always permits, so a canary placed there
//! would prove nothing. `canary_paths` pre-creates the canary's parent
//! directory host-side (the test process itself is unsandboxed), so the only
//! thing that can still block the write is the OS sandbox's permission check,
//! not a plain `FileNotFoundError` from a missing directory.
//!
//! Every scripted shell command below runs through `python3`, not raw shell
//! syntax (`echo ... > file`, `curl ...`, `sh script.sh`). This isn't a
//! stylistic choice: `SecurityPolicy` enforces its own app-layer command
//! allowlist and a handful of unconditional gates *before* a command ever
//! reaches the OS sandbox under test here (see
//! `crates/zeroclaw-config/src/policy.rs`'s `is_command_allowed` /
//! `command_risk_level`) -- `sh` and `curl` are not in the default allowlist
//! (`curl` is outright high-risk), and any `>` file redirect is blocked
//! unconditionally regardless of target. `python3` is both allowlisted and
//! low-risk by default, so a script invoked as `python3 foo.py` (no `-c`/`-m`,
//! which the policy does block) reaches real process execution, making the OS
//! sandbox - not an earlier app-layer gate - the thing actually under test.
#![cfg(unix)]

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroclaw_eval::live::{live_shell_sandbox, run_live_case};
use zeroclaw_eval::replay::TraceLlmProvider;
use zeroclaw_eval::{CaseProvider, LlmTrace, Mode, RunDeps};

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
/// process itself before the sandboxed subprocess runs: if the parent
/// directory didn't already exist, `open(canary, "w")` would raise
/// `FileNotFoundError` on every platform regardless of sandboxing, making the
/// test vacuous (it would "pass" even under `NoopSandbox`). Pre-creating the
/// directory means the *only* thing that can still block the write is the OS
/// sandbox's permission check.
fn canary_paths() -> (PathBuf, PathBuf) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let dir = base.join(format!("live-shell-canary-{}", std::process::id()));
    let file = dir.join("leaked.txt");
    std::fs::create_dir_all(&dir).unwrap();
    (dir, file)
}

/// Skip (rather than fail) on platforms with no OS sandbox backend at all,
/// but only where that is expected: this repo's default-features Linux CI
/// runs with neither Landlock (`sandbox-landlock` feature gated) nor Firejail
/// installed, so live-mode shell would always fail closed there. macOS (via
/// `sandbox-exec`) and Linux-with-`sandbox-landlock` must always have a real
/// backend, so a skip there would hide a real regression.
macro_rules! require_sandbox_or_skip {
    ($workspace:expr) => {
        match live_shell_sandbox($workspace) {
            Ok(sandbox) => sandbox,
            Err(_) => {
                assert!(
                    !cfg!(any(
                        target_os = "macos",
                        all(target_os = "linux", feature = "sandbox-landlock")
                    )),
                    "a real sandbox backend must exist on this platform"
                );
                eprintln!("skipping: no OS sandbox backend available on this platform");
                return;
            }
        }
    };
}

#[tokio::test]
async fn live_shell_cannot_write_outside_workspace() {
    // `canary_paths` pre-creates `canary_dir` host-side (unsandboxed) so the
    // directory's existence is never in question; only the sandbox's write
    // permission on `canary` itself is under test.
    let (_canary_dir, canary) = canary_paths();

    let tmp = tempfile::tempdir().unwrap();
    let _sandbox = require_sandbox_or_skip!(tmp.path());

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

    let deps = RunDeps {
        mode: Mode::Live,
        provider: Box::new(move |_trace: &LlmTrace| driver_provider(&driver)),
        live_tools: vec!["file_write".to_string(), "shell".to_string()],
        case_timeout: Duration::from_secs(10),
    };

    let record = run_live_case(&trace, &deps).await.unwrap();

    assert!(
        !canary.exists(),
        "sandbox breach: shell wrote outside the workspace to {}",
        canary.display()
    );
    // The escape attempt itself must be observable as a tool failure (the
    // sandboxed python process should exit non-zero on the denied write), not
    // just silently absent.
    assert!(
        !record.all_tools_succeeded,
        "the out-of-workspace write must not report success: {:?}",
        record.history
    );
}

#[tokio::test]
async fn live_shell_can_write_inside_workspace() {
    // Anti-vacuity: prove the sandbox permits ordinary in-workspace shell
    // writes, so `live_shell_cannot_write_outside_workspace` passing isn't
    // just the sandbox (or the test driver) failing to run anything at all.
    let tmp = tempfile::tempdir().unwrap();
    let _sandbox = require_sandbox_or_skip!(tmp.path());

    let trace: LlmTrace = serde_json::from_str(
        r#"{ "model_name": "shell-inside", "turns": [{ "user_input": "write inside" }], "tools": ["file_write", "shell"] }"#,
    )
    .unwrap();

    let driver = r#"{"model_name":"driver","turns":[{"user_input":"","steps":[
        {"response":{"type":"tool_calls","tool_calls":[{"id":"1","name":"file_write","arguments":{"path":"inside.py","content":"open(\"inside.txt\", \"w\").write(\"ok\")"}}]}},
        {"response":{"type":"tool_calls","tool_calls":[{"id":"2","name":"shell","arguments":{"command":"python3 inside.py"}}]}},
        {"response":{"type":"text","content":"done"}}
    ]}]}"#
        .to_string();

    let deps = RunDeps {
        mode: Mode::Live,
        provider: Box::new(move |_trace: &LlmTrace| driver_provider(&driver)),
        live_tools: vec!["file_write".to_string(), "shell".to_string()],
        case_timeout: Duration::from_secs(10),
    };

    let record = run_live_case(&trace, &deps).await.unwrap();
    assert!(
        record.all_tools_succeeded,
        "an in-workspace shell write must succeed under the sandbox: {:?}",
        record.history
    );
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn live_shell_cannot_reach_external_network() {
    // Landlock (as configured here; no AccessNet) does not restrict network
    // access at all, so this assertion only holds on the macOS Seatbelt
    // backend (deny-default network, DNS + localhost only). See the
    // "Residual host access" note in the threat-model doc.
    let tmp = tempfile::tempdir().unwrap();
    let _sandbox = require_sandbox_or_skip!(tmp.path());

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
        live_tools: vec!["file_write".to_string(), "shell".to_string()],
        case_timeout: Duration::from_secs(10),
    };

    let record = run_live_case(&trace, &deps).await.unwrap();
    assert!(
        !record.all_tools_succeeded,
        "the sandbox must deny outbound network access to a non-DNS, non-localhost address: {:?}",
        record.history
    );
}
