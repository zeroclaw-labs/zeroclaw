use std::path::Path;
use std::process::{Command, Output};

use zeroclaw_config::agent_bundle::{CONFIG_FILE, MANIFEST_FILE, SKILLS_DIR, WORKSPACE_DIR};

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn export(config_dir: &Path, out: &Path, force: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env_remove("ZEROCLAW_DATA_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .env("RUST_LOG", "off")
        .arg("--config-dir")
        .arg(config_dir)
        .args(["agents", "export", "export_fixture", "--out"])
        .arg(out);
    if force {
        command.arg("--force");
    }
    command.output().expect("run zeroclaw agents export")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "export failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn agents_export_copies_workspace_and_skills_and_replaces_only_with_force() {
    let install = tempfile::tempdir().unwrap();
    write(
        &install.path().join("config.toml"),
        &format!(
            r#"schema_version = {}
locale = "en"

[providers.models.anthropic.fixture]

[risk_profiles.guarded]

[skill_bundles.fixture]

[agents.export_fixture]
model_provider = "anthropic.fixture"
risk_profile = "guarded"
skill_bundles = ["fixture"]
"#,
            zeroclaw_config::migration::CURRENT_SCHEMA_VERSION
        ),
    );
    let workspace = install.path().join("agents/export_fixture/workspace");
    write(&workspace.join("notes/plan.md"), "workspace note");
    write(&workspace.join("memory/brain.db"), "excluded memory");
    write(
        &install.path().join("shared/skills/fixture/search/SKILL.md"),
        "# Search fixture",
    );
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("bundle");

    assert_success(&export(install.path(), &out, false));
    assert!(out.join(CONFIG_FILE).is_file());
    assert!(out.join(MANIFEST_FILE).is_file());
    assert_eq!(
        std::fs::read_to_string(out.join(WORKSPACE_DIR).join("notes/plan.md")).unwrap(),
        "workspace note"
    );
    assert_eq!(
        std::fs::read_to_string(out.join(SKILLS_DIR).join("fixture/search/SKILL.md")).unwrap(),
        "# Search fixture"
    );
    assert!(!out.join(WORKSPACE_DIR).join("memory").exists());

    write(&out.join("old-only.txt"), "previous bundle");
    write(&workspace.join("notes/plan.md"), "updated workspace note");
    let refused = export(install.path(), &out, false);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not empty"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(out.join("old-only.txt")).unwrap(),
        "previous bundle"
    );
    assert_eq!(
        std::fs::read_to_string(out.join(WORKSPACE_DIR).join("notes/plan.md")).unwrap(),
        "workspace note"
    );

    assert_success(&export(install.path(), &out, true));
    assert!(!out.join("old-only.txt").exists());
    assert_eq!(
        std::fs::read_to_string(out.join(WORKSPACE_DIR).join("notes/plan.md")).unwrap(),
        "updated workspace note"
    );
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("memory/brain.db")).unwrap(),
        "excluded memory"
    );
}
