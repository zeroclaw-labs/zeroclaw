//! Regression: `zeroclaw sop list` surfaces malformed SOP.toml failures (#9786).
//!
//! The in-crate `load_sops_from_directory_report` helper alone still passes if
//! every change to `sop list` is removed. These tests spawn the shipped binary
//! and assert what an operator observes: non-zero exit, the failing directory
//! name, and the parse reason on stdout/stderr.

use std::path::Path;
use std::process::{Command, Output};

/// Pin English so assertions on Fluent strings stay stable.
const CONFIG_TOML: &str = r#"schema_version = 3
locale = "en"
"#;

fn run(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn sops_root(config_dir: &Path) -> std::path::PathBuf {
    // `install_root` is the config dir; the default SOP tree is
    // `<install>/shared/sops`.
    config_dir.join("shared").join("sops")
}

fn write_malformed(sops_root: &Path, name: &str) {
    let bad = sops_root.join(name);
    std::fs::create_dir_all(&bad).expect("create malformed sop dir");
    std::fs::write(
        bad.join("SOP.toml"),
        format!(
            r#"[sop]
name = "{name}"
description = "unknown field on a trigger"
version = "1.0.0"

[[triggers]]
type = "channel"
topic = "git.main:pull_request.opened"
"#
        ),
    )
    .expect("write malformed SOP.toml");
    std::fs::write(bad.join("SOP.md"), "- next: done\n").expect("write SOP.md");
}

fn write_valid(sops_root: &Path, name: &str) {
    let good = sops_root.join(name);
    std::fs::create_dir_all(&good).expect("create valid sop dir");
    std::fs::write(
        good.join("SOP.toml"),
        format!(
            r#"[sop]
name = "{name}"
description = "a valid SOP"
version = "1.0.0"

[[triggers]]
type = "manual"
"#
        ),
    )
    .expect("write valid SOP.toml");
    std::fs::write(
        good.join("SOP.md"),
        r#"# Valid

## Steps

1. **Do work** — Something.
"#,
    )
    .expect("write SOP.md");
}

#[test]
fn sop_list_malformed_only_exits_nonzero_and_names_failure() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let config_dir = dir.path();
    std::fs::write(config_dir.join("config.toml"), CONFIG_TOML).expect("write config");

    let root = sops_root(config_dir);
    write_malformed(&root, "repro-bad-trigger");

    let out = run(config_dir, &["sop", "list"]);
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "malformed-only sop list must exit non-zero:\n{text}"
    );
    assert!(
        text.contains("repro-bad-trigger"),
        "failure must name the directory:\n{text}"
    );
    assert!(
        text.contains("failed to load"),
        "failure must carry the load diagnostic:\n{text}"
    );
    assert!(
        text.contains("failed to load") && text.len() > "repro-bad-trigger".len() + 20,
        "failure line must include a parse reason after the directory:\n{text}"
    );
    assert!(
        text.contains("1 SOP(s) failed to load"),
        "summary must report the failure count:\n{text}"
    );
}

#[test]
fn sop_list_mixed_valid_and_malformed_exits_nonzero_and_shows_both() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let config_dir = dir.path();
    std::fs::write(config_dir.join("config.toml"), CONFIG_TOML).expect("write config");

    let root = sops_root(config_dir);
    write_valid(&root, "good-sop");
    write_malformed(&root, "repro-bad-trigger");

    let out = run(config_dir, &["sop", "list"]);
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "mixed sop list must exit non-zero when any SOP fails to load:\n{text}"
    );
    assert!(
        text.contains("good-sop"),
        "valid SOP must still be listed:\n{text}"
    );
    assert!(
        text.contains("repro-bad-trigger"),
        "failure must name the malformed directory:\n{text}"
    );
    assert!(
        text.contains("failed to load"),
        "failure must carry the load diagnostic:\n{text}"
    );
    assert!(
        text.contains("1 SOP(s) failed to load"),
        "summary must report the failure count:\n{text}"
    );
}
