//! The `zeroclaw` CLI must refuse to run when an agent's shell spawned it.
//!
//! The CLI carries the operator's authority — it loads, decrypts and rewrites
//! `config.toml`, and no agent policy exists anywhere in the process. So an
//! agent that shells out to it escalates to the operator in one call:
//! `zeroclaw config set agents.x.risk_profile yolo`.
//!
//! Nothing in the risk profile stops that call. `balanced` — the default preset
//! — allows any command name, and `zeroclaw` is classified low-risk, so it
//! passes the allowlist, the risk gate and the path check cleanly. See
//! `zeroclaw_config::presets` for the matrix that pins this.
//!
//! These run the real binary rather than the guard function, because the value
//! of the guard is entirely in *where* it sits: before config load, before
//! logging, before any subcommand dispatch.

use std::process::{Command, Output, Stdio};

fn run_cli(args: &[&str], agent_marker: Option<&str>) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("RUST_LOG", "off")
        .env_remove(zeroclaw_api::AGENT_SHELL_ENV_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(marker) = agent_marker {
        cmd.env(zeroclaw_api::AGENT_SHELL_ENV_VAR, marker);
    }
    cmd.output().expect("zeroclaw binary should spawn")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn config_set_is_refused_when_spawned_by_an_agent_shell() {
    let output = run_cli(
        &["config", "set", "agents.helper.risk_profile", "yolo"],
        Some("1"),
    );

    assert!(
        !output.status.success(),
        "the escalating command must fail, got success"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("operator authority"),
        "refusal should say why, got: {stderr}"
    );
}

#[test]
fn every_subcommand_is_refused_not_just_the_mutating_ones() {
    // `config get` on a secret decrypts it, so reads exfiltrate as surely as
    // writes escalate. An allowlist of "safe" subcommands is one new subcommand
    // away from being wrong.
    for args in [
        vec!["config", "get", "agents.helper.model_provider"],
        vec!["config", "list"],
        vec!["agent", "-a", "helper", "-m", "hello"],
    ] {
        let output = run_cli(&args, Some("1"));
        assert!(
            !output.status.success(),
            "`{}` must be refused from an agent shell",
            args.join(" ")
        );
        assert!(
            stderr_of(&output).contains("operator authority"),
            "`{}` should fail with the refusal, not something incidental",
            args.join(" ")
        );
    }
}

#[test]
fn an_operator_terminal_is_unaffected() {
    // The same command without the marker must not hit the guard. It may still
    // fail for its own reasons (no config in this environment), so assert on
    // the absence of the refusal rather than on success.
    let output = run_cli(&["config", "get", "agents.helper.model_provider"], None);

    assert!(
        !stderr_of(&output).contains("operator authority"),
        "an operator's own terminal must never see the agent refusal: {}",
        stderr_of(&output)
    );
}

#[test]
fn an_empty_marker_reads_as_absent() {
    // A stray `export ZEROCLAW_AGENT_SHELL=` must not lock an operator out.
    let output = run_cli(&["config", "get", "agents.helper.model_provider"], Some(""));

    assert!(
        !stderr_of(&output).contains("operator authority"),
        "empty marker should not trip the guard: {}",
        stderr_of(&output)
    );
}
