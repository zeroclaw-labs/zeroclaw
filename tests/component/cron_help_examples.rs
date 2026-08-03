//! CLI-boundary regression for the corrected `cron` help examples (#9672).
//!
//! The reported bug: the examples printed by `cron --help` / `cron add --help`
//! and the empty `cron list` hint could not be run as printed — the prompt
//! examples were missing `--prompt` and the shell example carried no
//! `--agent` value. These tests spawn the real binary and assert the printed
//! forms stay runnable, so the fix cannot silently regress in the clap
//! `long_about` strings or the Fluent empty-state hint.

use std::process::Command;

const PROMPT_EXAMPLE_TZ: &str = "zeroclaw cron add '0 9 * * 1-5' 'Good morning' --agent sentinel --prompt --tz America/New_York";
const PROMPT_EXAMPLE: &str =
    "zeroclaw cron add '*/30 * * * *' 'Check system health' --agent sentinel --prompt";
const SHELL_EXAMPLE: &str = "zeroclaw cron add '*/5 * * * *' 'echo ok' --agent sentinel";

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .expect("failed to run zeroclaw binary")
}

#[test]
fn cron_parent_help_shows_runnable_examples() {
    let out = run(&["cron", "--help"]);
    assert!(
        out.status.success(),
        "cron --help failed: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for example in [PROMPT_EXAMPLE_TZ, PROMPT_EXAMPLE, SHELL_EXAMPLE] {
        assert!(
            stdout.contains(example),
            "cron --help must show a runnable example, missing: {example}"
        );
    }
}

#[test]
fn cron_add_help_shows_runnable_examples() {
    let out = run(&["cron", "add", "--help"]);
    assert!(
        out.status.success(),
        "cron add --help failed: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for example in [PROMPT_EXAMPLE_TZ, PROMPT_EXAMPLE, SHELL_EXAMPLE] {
        assert!(
            stdout.contains(example),
            "cron add --help must show a runnable example, missing: {example}"
        );
    }
}

#[test]
fn cron_list_empty_state_shows_runnable_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", tmp.path())
        .env("RUST_LOG", "off")
        .args(["cron", "list"])
        .output()
        .expect("failed to run zeroclaw cron list");
    assert!(
        out.status.success(),
        "cron list with no jobs must succeed: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No scheduled tasks yet."),
        "empty cron list must show the empty-state notice, got: {stdout}"
    );
    assert!(
        stdout.contains("zeroclaw cron add '0 9 * * *' 'echo ok' --agent sentinel"),
        "empty cron list hint must be runnable (agent value present), got: {stdout}"
    );
}
