//! CLI-boundary regression for the corrected `cron` help examples.
//!
//! The reported bug: the examples printed by `cron --help` / `cron add --help`
//! and the empty `cron list` hint could not be run as printed — the prompt
//! examples were missing `--prompt` and the shell example carried no
//! `--agent` value. These tests spawn the real binary and assert the printed
//! forms stay runnable, so the fix cannot silently regress in the clap
//! `long_about` strings or the Fluent empty-state hint.
//!
//! Every invocation is locale-hermetic: the binary runs against an isolated
//! config dir whose `locale = "en"`, so the help and empty-state output are
//! asserted in English regardless of the developer's environment locale.

use std::path::Path;
use std::process::{Command, Output};

const PROMPT_EXAMPLE_TZ: &str = "zeroclaw cron add '0 9 * * 1-5' 'Good morning' --agent sentinel --prompt --tz America/New_York";
const PROMPT_EXAMPLE: &str =
    "zeroclaw cron add '*/30 * * * *' 'Check system health' --agent sentinel --prompt";
const SHELL_EXAMPLE: &str = "zeroclaw cron add '*/5 * * * *' 'echo ok' --agent sentinel";

/// Isolated config dir that pins the CLI locale to English.
fn english_config_dir(tmp: &Path) {
    std::fs::write(tmp.join("config.toml"), "locale = \"en\"\n").unwrap();
}

fn run(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("failed to run zeroclaw binary")
}

fn assert_success(out: &Output) -> String {
    assert!(
        out.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn cron_parent_help_shows_runnable_examples() {
    let tmp = tempfile::tempdir().unwrap();
    english_config_dir(tmp.path());
    let stdout = assert_success(&run(tmp.path(), &["cron", "--help"]));
    for example in [PROMPT_EXAMPLE_TZ, PROMPT_EXAMPLE, SHELL_EXAMPLE] {
        assert!(
            stdout.contains(example),
            "cron --help must show a runnable example, missing: {example}"
        );
    }
}

#[test]
fn cron_add_help_shows_runnable_examples() {
    let tmp = tempfile::tempdir().unwrap();
    english_config_dir(tmp.path());
    let stdout = assert_success(&run(tmp.path(), &["cron", "add", "--help"]));
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
    english_config_dir(tmp.path());
    let stdout = assert_success(&run(tmp.path(), &["cron", "list"]));
    assert!(
        stdout.contains("No scheduled tasks yet."),
        "empty cron list must show the empty-state notice, got: {stdout}"
    );
    assert!(
        stdout.contains("zeroclaw cron add '0 9 * * *' 'echo ok' --agent sentinel"),
        "empty cron list hint must be runnable (agent value present), got: {stdout}"
    );
}
