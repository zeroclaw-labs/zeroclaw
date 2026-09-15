//! `models status` and `zeroclaw status` must perceive models hosted as
//! nested entries under a provider profile.
//!
//! A profile whose models live in a `models.<alias>` subtable (the
//! multi-model shape this PR introduces) previously showed up as "(none)"
//! in `zeroclaw status` and could make `models status` report "no model
//! configured" even though every entry carried an id. Both surfaces now
//! enumerate through `Config::configured_model_entries` — the same
//! enumeration `models list` and `doctor` use — so they cannot disagree
//! about what a profile hosts.
//!
//! These run the real binary because the claim is about what an operator
//! sees on the CLI surface.

use std::process::Command;

const CONFIG_WITH_DEFAULT: &str = r#"schema_version = 5

[providers.models.openai.gw]
uri = "http://127.0.0.1:1"
api_key = "test-key"

[providers.models.openai.gw.models.default]
id = "gpt-4.1"

[providers.models.openai.gw.models.cheap]
id = "gpt-4o-mini"
"#;

const CONFIG_NO_DEFAULT: &str = r#"schema_version = 5

[providers.models.openai.gw]
uri = "http://127.0.0.1:1"
api_key = "test-key"

[providers.models.openai.gw.models.cheap]
id = "gpt-4o-mini"

[providers.models.openai.gw.models.big]
id = "gpt-4o"
"#;

fn run(config_dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run zeroclaw {:?}: {e}", args));
    // The commands print their report regardless of the overall exit status
    // (doctor-style diagnostics report, not a pass/fail), so assert on the
    // captured stdout either way but fail loudly on unexpected crashes.
    assert!(
        out.status.success(),
        "zeroclaw {:?} failed: {}\nstdout:\n{}\nstderr:\n{}",
        args,
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn models_status_prefers_the_default_entry_of_a_nested_profile() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    std::fs::write(dir.path().join("config.toml"), CONFIG_WITH_DEFAULT).expect("write config.toml");

    let stdout = run(dir.path(), &["models", "status"]);
    assert!(
        stdout.contains("gpt-4.1"),
        "the profile's default entry is the reported model, got:\n{stdout}"
    );
    assert!(
        stdout.contains("openai.gw"),
        "the provider ref must identify the profile, got:\n{stdout}"
    );
}

#[test]
fn models_status_reports_a_nested_only_profile_without_a_default() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    std::fs::write(dir.path().join("config.toml"), CONFIG_NO_DEFAULT).expect("write config.toml");

    // No `models.default` and multiple entries: the first enumerated entry
    // (deterministic sorted order → `big`) is reported. The claim under test
    // is that a nested-only profile is *something*, not "none".
    let stdout = run(dir.path(), &["models", "status"]);
    assert!(
        stdout.contains("gpt-4o") && stdout.contains("openai.gw.big"),
        "a nested-only profile reports its first enumerated entry, got:\n{stdout}"
    );
}

#[test]
fn zeroclaw_status_lists_each_nested_model_entry() {
    let dir = tempfile::TempDir::new().expect("temp config dir");
    std::fs::write(dir.path().join("config.toml"), CONFIG_NO_DEFAULT).expect("write config.toml");

    let stdout = run(dir.path(), &["status"]);
    assert!(
        stdout.contains("openai.gw"),
        "the profile must appear in the provider block, got:\n{stdout}"
    );
    for (alias, id) in [("cheap", "gpt-4o-mini"), ("big", "gpt-4o")] {
        assert!(
            stdout.contains(alias) && stdout.contains(id),
            "status must list entry {alias} with id {id}, got:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("(none)"),
        "a nested-only profile must not render as '(none)':\n{stdout}"
    );
}
