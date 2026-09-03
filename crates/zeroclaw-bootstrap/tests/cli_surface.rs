//! The command-line surface itself is a security control.
//!
//! These tests assert the *absence* of arguments. If someone later adds a
//! `--url`, `--install-root`, `--asset`, `--command`, or `--config` flag to any
//! subcommand, the corresponding assertion here fails.
//!
//! Every case runs without network: unknown arguments are rejected by the
//! argument parser, and the one real operation exercised (`status`) makes no
//! requests.

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_zeroclaw-bootstrap");

/// Every argument name that would defeat the launcher's refusals.
const FORBIDDEN_ARGUMENTS: &[&str] = &[
    "--url",
    "--source-url",
    "--download-url",
    "--origin",
    "--mirror",
    "--repo",
    "--install-root",
    "--install-dir",
    "--prefix",
    "--target-dir",
    "--asset",
    "--asset-name",
    "--artifact",
    "--command",
    "--exec",
    "--shell",
    "--script",
    "--config",
    "--config-dir",
    "--config-file",
    "--target",
];

/// Subcommands, with the minimum extra arguments needed to reach parsing of
/// the argument under test.
const SUBCOMMANDS: &[&[&str]] = &[
    &["status"],
    &["plan"],
    &["install", "--approve", "sha256:deadbeef"],
    &["handoff", "--verify-only"],
];

fn run(args: &[&str]) -> Output {
    let root = tempfile::tempdir().expect("temp home");
    Command::new(BIN)
        .args(args)
        // Keep the run hermetic: never touch the developer's real install.
        .env("CARGO_HOME", root.path().join("cargo"))
        .env("HOME", root.path().join("home"))
        .env("USERPROFILE", root.path().join("profile"))
        .output()
        .expect("launcher runs")
}

#[test]
fn no_subcommand_accepts_a_url_root_asset_command_or_config_argument() {
    for subcommand in SUBCOMMANDS {
        for forbidden in FORBIDDEN_ARGUMENTS {
            let mut args: Vec<&str> = subcommand.to_vec();
            args.push(forbidden);
            args.push("https://evil.example/payload");

            let output = run(&args);
            assert!(
                !output.status.success(),
                "`{}` accepted `{forbidden}` — the launcher must expose no such argument",
                args.join(" ")
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("unexpected argument") || stderr.contains("unexpected value"),
                "`{}` must be rejected by the argument parser, got:\n{stderr}",
                args.join(" ")
            );
        }
    }
}

#[test]
fn no_positional_argument_is_accepted_anywhere() {
    for subcommand in SUBCOMMANDS {
        let mut args: Vec<&str> = subcommand.to_vec();
        args.push("https://evil.example/payload");

        let output = run(&args);
        assert!(
            !output.status.success(),
            "`{}` accepted a positional argument",
            args.join(" ")
        );
    }
}

#[test]
fn an_unknown_subcommand_is_refused() {
    for unknown in ["download", "run", "exec", "configure", "update"] {
        let output = run(&[unknown]);
        assert!(
            !output.status.success(),
            "unknown subcommand `{unknown}` must be refused"
        );
    }
}

#[test]
fn install_without_an_approval_token_refuses_before_any_network_access() {
    let output = run(&["install"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("install requires --approve"),
        "expected the approval refusal, got:\n{stderr}"
    );
}

#[test]
fn a_malformed_release_tag_is_refused_before_any_network_access() {
    for hostile in ["../../evil", "https://evil.example/x", "v1/../../x"] {
        let output = run(&["plan", "--tag", hostile]);
        assert!(!output.status.success(), "tag `{hostile}` must be refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("is not acceptable"),
            "tag `{hostile}` gave:\n{stderr}"
        );
    }
}

#[test]
fn status_runs_without_network_and_reports_the_host() {
    let output = run(&["status"]);
    assert!(
        output.status.success(),
        "status failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Bootstrap status"));
    assert!(stdout.contains("host target"));
    assert!(stdout.contains("existing binary   none"));
}

#[test]
fn the_four_operations_are_the_entire_surface() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);

    for expected in ["status", "plan", "install", "handoff"] {
        assert!(
            help.contains(expected),
            "`{expected}` missing from --help:\n{help}"
        );
    }
    // A command that would give the launcher management authority must not
    // appear.
    for forbidden in ["config", "provider", "agent", "serve", "daemon"] {
        assert!(
            !help.contains(&format!("  {forbidden}")),
            "`{forbidden}` must not be a bootstrap operation:\n{help}"
        );
    }
}

#[test]
fn the_help_text_states_what_the_launcher_refuses() {
    let output = run(&["--help"]);
    let help = String::from_utf8_lossy(&output.stdout);
    for claim in ["download URL", "install root", "config.toml", "--approve"] {
        assert!(
            help.contains(claim),
            "--help must state the {claim:?} refusal:\n{help}"
        );
    }
}
