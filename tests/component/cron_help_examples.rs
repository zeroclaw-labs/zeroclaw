//! CLI-boundary regression for the corrected `cron` help examples.
//!
//! The reported bug: the examples printed by `cron --help` / `cron add --help`
//! and the empty `cron list` hint could not be run as printed — the prompt
//! examples were missing `--prompt` and the shell example carried no
//! `--agent` value. These tests spawn the real binary and assert the printed
//! forms stay runnable, so the fix cannot silently regress in the clap
//! `long_about` strings or the Fluent empty-state hint.
//! `add-at` timestamps are parsed and checked against the current time because
//! production rejects one-shot schedules that are not in the future.
//!
//! Every invocation is locale-hermetic: the binary runs against an isolated
//! config dir whose `locale = "en"`, so the help and empty-state output are
//! asserted in English regardless of the developer's environment locale.

use std::path::Path;
use std::process::{Command, Output};

use chrono::{DateTime, Utc};

const PROMPT_EXAMPLE_TZ: &str = "zeroclaw cron add '0 9 * * 1-5' 'Good morning' --agent sentinel --prompt --tz America/New_York";
const PROMPT_EXAMPLE: &str =
    "zeroclaw cron add '*/30 * * * *' 'Check system health' --agent sentinel --prompt";
const SHELL_EXAMPLE: &str = "zeroclaw cron add '*/5 * * * *' 'echo ok' --agent sentinel";
const ADD_EVERY_PARENT_EXAMPLE: &str =
    "zeroclaw cron add-every 60000 'Ping heartbeat' --agent sentinel --prompt";
const ONCE_PARENT_EXAMPLE: &str =
    "zeroclaw cron once 30m 'Run backup in 30 minutes' --agent sentinel --prompt";

const ADD_EVERY_CHILD_EXAMPLES: &[&str] = &[
    "zeroclaw cron add-every --agent triage --prompt 60000 'Ping heartbeat'",
    "zeroclaw cron add-every --agent triage --prompt 3600000 'Hourly report'",
];
const ONCE_CHILD_EXAMPLES: &[&str] = &[
    "zeroclaw cron once --agent ops-bot --prompt 30m 'Run backup in 30 minutes'",
    "zeroclaw cron once --agent researcher --prompt 2h 'Follow up on deployment'",
];

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

fn add_at_examples(help: &str) -> Vec<&str> {
    help.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("zeroclaw cron add-at "))
        .collect()
}

fn assert_add_at_examples_are_future(help: &str, expected_count: usize) {
    let examples = add_at_examples(help);
    assert_eq!(
        examples.len(),
        expected_count,
        "expected {expected_count} add-at examples, got: {examples:?}"
    );

    let now = Utc::now();
    for example in examples {
        let timestamp = example
            .split_ascii_whitespace()
            .find_map(|part| DateTime::parse_from_rfc3339(part).ok())
            .unwrap_or_else(|| panic!("add-at example is missing an RFC3339 timestamp: {example}"));
        assert!(
            timestamp > now,
            "add-at example timestamp must satisfy the production future-time requirement: {example}"
        );
    }
}

#[test]
fn cron_parent_help_shows_runnable_examples() {
    let tmp = tempfile::tempdir().unwrap();
    english_config_dir(tmp.path());
    let stdout = assert_success(&run(tmp.path(), &["cron", "--help"]));
    for example in [
        PROMPT_EXAMPLE_TZ,
        PROMPT_EXAMPLE,
        SHELL_EXAMPLE,
        ADD_EVERY_PARENT_EXAMPLE,
        ONCE_PARENT_EXAMPLE,
    ] {
        assert!(
            stdout.contains(example),
            "cron --help must show a runnable example, missing: {example}"
        );
    }
    let add_at_examples = add_at_examples(&stdout);
    assert!(
        add_at_examples
            .iter()
            .any(|example| example.ends_with("'Send reminder' --agent sentinel --prompt")),
        "cron --help must show a runnable add-at example, got: {stdout}"
    );
    assert_add_at_examples_are_future(&stdout, 1);
}

#[test]
fn cron_one_shot_and_interval_help_show_runnable_prompt_examples() {
    let tmp = tempfile::tempdir().unwrap();
    english_config_dir(tmp.path());

    let add_at_stdout = assert_success(&run(tmp.path(), &["cron", "add-at", "--help"]));
    let add_at_examples = add_at_examples(&add_at_stdout);
    for expected in ["'Send reminder'", "'Happy New Year!'"] {
        assert!(
            add_at_examples.iter().any(|example| {
                example.starts_with("zeroclaw cron add-at --agent morning-shift --prompt ")
                    && example.ends_with(expected)
            }),
            "cron add-at --help must show a runnable prompt example containing {expected}, got: {add_at_stdout}"
        );
    }
    assert_add_at_examples_are_future(&add_at_stdout, 2);

    for (subcommand, examples) in [
        ("add-every", ADD_EVERY_CHILD_EXAMPLES),
        ("once", ONCE_CHILD_EXAMPLES),
    ] {
        let stdout = assert_success(&run(tmp.path(), &["cron", subcommand, "--help"]));
        for example in examples {
            assert!(
                stdout.contains(example),
                "cron {subcommand} --help must show a runnable prompt example, missing: {example}"
            );
        }
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
