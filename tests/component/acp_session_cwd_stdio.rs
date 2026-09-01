//! CLI front-door proof for the ACP stdio surface.
//!
//! The unit tests in `crates/zeroclaw-channels/.../acp_server.rs` drive
//! `handle_session_new` / `serve_reader` directly; this test crosses the real
//! `zeroclaw acp` **process boundary** required by the repository's
//! User-boundary-proof contract: it launches the shipped binary, feeds
//! newline-delimited JSON-RPC through the process's real stdin, reads real
//! stdout, and asserts that an omitted-`cwd` `session/new` returns the
//! per-agent workspace — not the daemon process CWD. No provider API key and no
//! network are needed: `session/new` builds the agent workspace and returns
//! before any model call.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Minimal config with a single dispatchable agent `test-agent`. Mirrors the
/// runtime unit helper `make_test_config`: an Anthropic provider profile (no
/// key), the `default` risk/runtime profiles, and an agent that references them.
/// `[agents.test-agent]` needs no explicit `workspace`, so its workspace
/// resolves to `<install-root>/agents/test-agent/workspace`, where the install
/// root is the directory holding `config.toml` (i.e. `ZEROCLAW_CONFIG_DIR`).
///
/// `schema_version = 3` is REQUIRED: `Config::load_or_init` treats a config with
/// no `schema_version` as V1 (`detect_version` → 1) and runs the V1→V3 migration,
/// which reshapes `[providers.models.*]` and drops the already-V3 `model` field —
/// so `session/new` would then fail with "model_provider … no model set". Declaring
/// the current schema version skips the migration, exactly as a real V3 install does.
const TEST_CONFIG: &str = r#"
schema_version = 3

[providers.models.anthropic.default]
model = "claude-haiku-4-5"

[risk_profiles.default]

[runtime_profiles.default]

[agents.test-agent]
model_provider = "anthropic.default"
risk_profile = "default"
runtime_profile = "default"
"#;

#[test]
fn acp_stdio_session_new_omitted_cwd_returns_agent_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_dir = dir.path();
    std::fs::write(config_dir.join("config.toml"), TEST_CONFIG).expect("write config.toml");

    // Launch the real `zeroclaw acp` command against the isolated config/home.
    // stderr is discarded (logs go there); stdout carries only JSON-RPC frames.
    let mut child = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .arg("acp")
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .env_remove("ZEROCLAW_WORKSPACE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `zeroclaw acp`");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");

    // Drain stdout on a worker thread so the main thread can enforce a deadline
    // and never block the process boundary if the server misbehaves.
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // `initialize`, then an omitted-`cwd` `session/new` for the configured agent.
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
        .expect("write initialize");
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\
              \"params\":{\"agentAlias\":\"test-agent\"}}\n",
        )
        .expect("write session/new");
    stdin.flush().expect("flush stdin");

    // Collect frames until the `session/new` (id=2) reply arrives or we time out.
    // Non-JSON lines (should be none — logs are on stderr) are tolerated.
    let mut workspace_dir: Option<String> = None;
    let mut session_error: Option<String> = None;
    let start = Instant::now();
    let deadline = Duration::from_secs(30);
    while start.elapsed() < deadline {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(line) => {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if value.get("id").and_then(serde_json::Value::as_i64) == Some(2) {
                    if let Some(err) = value.get("error") {
                        session_error = Some(err.to_string());
                    }
                    workspace_dir = value
                        .get("result")
                        .and_then(|r| r.get("workspaceDir"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Closing stdin sends EOF so the serve loop exits; kill is a backstop.
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();

    assert!(
        session_error.is_none(),
        "session/new returned an error: {}",
        session_error.unwrap_or_default()
    );
    let workspace_dir = workspace_dir
        .expect("session/new must return a workspaceDir over real stdio before timeout");

    // The omitted-cwd session must root at the per-agent workspace
    // (`<install-root>/agents/test-agent/workspace`), not the daemon CWD.
    let ws = std::path::Path::new(&workspace_dir);
    assert!(
        ws.ends_with("agents/test-agent/workspace"),
        "omitted-cwd session/new must return the per-agent workspace, got: {workspace_dir}"
    );
}
