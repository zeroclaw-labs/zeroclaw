//! `config migrate` must surface load-time IgnoredEnvOverride notices.
//!
//! Regression: startup skips migration_notices when the command is
//! `config migrate` (it reports its own file-change notices), but retired
//! ZEROCLAW_* env overrides never appear in migrate_file_in_place's report.

use std::process::{Command, Output};

fn run_zeroclaw(config_dir: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    let mut cmd = Command::new(bin);
    cmd.env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run zeroclaw")
}

/// Shape that `migrate_file_in_place` treats as already current (see its noop test).
const CURRENT_CONFIG: &str = "schema_version = 4\n[tools]\noptional = []\n";

#[test]
fn config_migrate_already_current_prints_retired_env_warning() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    std::fs::write(config_dir.path().join("config.toml"), CURRENT_CONFIG).expect("write config");

    let retired = "ZEROCLAW_runtime_profiles__fast__context_compression__summary_model";
    let out = run_zeroclaw(
        config_dir.path(),
        &["config", "migrate"],
        &[(retired, "haiku")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "migrate should succeed: status={:?} stdout={stdout} stderr={stderr}",
        out.status
    );
    assert!(
        stdout.contains("already at current schema") || stdout.contains("Config already"),
        "expected already-current message, got stdout={stdout}"
    );
    assert!(
        !stdout.contains("Migrated "),
        "fixture must stay already-current, got stdout={stdout}"
    );
    assert!(
        stderr.contains(retired) && stderr.to_ascii_lowercase().contains("ignored"),
        "expected IgnoredEnvOverride on stderr once, got stderr={stderr}"
    );
    assert_eq!(
        stderr.matches(retired).count(),
        1,
        "warning must print once, got stderr={stderr}"
    );
}

#[test]
fn config_migrate_json_already_current_includes_env_notice() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    std::fs::write(config_dir.path().join("config.toml"), CURRENT_CONFIG).expect("write config");

    let retired = "ZEROCLAW_runtime_profiles__fast__context_compression__summary_model";
    let out = run_zeroclaw(
        config_dir.path(),
        &["config", "migrate", "--json"],
        &[(retired, "haiku")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "migrate --json should succeed: status={:?} stdout={stdout} stderr={stderr}",
        out.status
    );
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("migrate --json must emit JSON");
    assert_eq!(value["migrated"], false);
    let notices = value["notices"]
        .as_array()
        .expect("notices array on already-current JSON");
    assert!(
        notices
            .iter()
            .any(|n| { n["kind"] == "ignored_env_override" && n["variable"] == retired }),
        "expected ignored_env_override for {retired}, got {notices:?}"
    );
}
