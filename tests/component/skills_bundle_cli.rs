use std::process::{Command, Output};

fn run_zeroclaw(config_dir: &std::path::Path, args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

#[test]
fn skills_install_bundle_then_list_agent_shows_runtime_view() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    std::fs::write(
        config_dir.path().join("config.toml"),
        r#"schema_version = 3

[skill_bundles.smoke]

[agents.default]
enabled = true
skill_bundles = ["smoke"]
"#,
    )
    .expect("write config");

    let source_root = tempfile::tempdir().expect("temp skill source root");
    let source_skill = source_root.path().join("release-check");
    std::fs::create_dir_all(&source_skill).expect("create skill source");
    std::fs::write(
        source_skill.join("SKILL.md"),
        r#"---
name: release-check
description: Check release readiness
version: 0.1.0
tags: [release]
---

# Release check

Review release readiness before signoff.
"#,
    )
    .expect("write skill");

    let source_arg = source_skill.to_string_lossy().to_string();
    let install = run_zeroclaw(
        config_dir.path(),
        &["skills", "install", &source_arg, "--bundle", "smoke"],
    );
    assert!(
        install.status.success(),
        "install should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let installed_skill = config_dir
        .path()
        .join("shared")
        .join("skills")
        .join("smoke")
        .join("release-check")
        .join("SKILL.md");
    assert!(
        installed_skill.is_file(),
        "skill should be installed in the bundle directory: {}",
        installed_skill.display()
    );

    let list = run_zeroclaw(config_dir.path(), &["skills", "list", "--agent", "default"]);
    assert!(
        list.status.success(),
        "list should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );

    let stdout = String::from_utf8(list.stdout).expect("stdout utf8");
    assert!(
        stdout.contains("release-check"),
        "agent runtime view should include installed skill:\n{stdout}"
    );
}
