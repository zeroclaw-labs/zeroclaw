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
        .current_dir(config_dir)
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

fn write_config(config_dir: &Path, workspace: Option<&Path>) {
    let mut config = format!(
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
    );
    if let Some(workspace) = workspace {
        config.push_str(&format!(
            "\n[agents.export_fixture.workspace]\npath = {}\n",
            toml::Value::String(workspace.to_str().unwrap().to_string())
        ));
    }
    write(&config_dir.join("config.toml"), &config);
}

#[test]
fn agents_export_copies_workspace_and_skills_and_replaces_only_with_force() {
    let install = tempfile::tempdir().unwrap();
    write_config(install.path(), None);
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

#[test]
fn agents_export_refuses_unresolvable_workspace_parents_without_replacing_the_bundle() {
    let install = tempfile::tempdir().unwrap();
    write(&install.path().join("workspace/notes.md"), "workspace note");
    write(&install.path().join("not-a-directory"), "ordinary file");
    std::fs::create_dir(install.path().join("existing")).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("bundle");
    write(&out.join("keep.txt"), "previous bundle");

    for configured in [
        "missing/../workspace",
        "not-a-directory/../workspace",
        "existing/../missing/../workspace",
    ] {
        write_config(install.path(), Some(Path::new(configured)));
        let output = export(install.path(), &out, true);
        assert!(!output.status.success(), "accepted {configured}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("before `..`"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(out.join("keep.txt")).unwrap(),
            "previous bundle"
        );
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
        assert!(!install.path().join("missing").exists());
        assert_eq!(
            std::fs::read_to_string(install.path().join("workspace/notes.md")).unwrap(),
            "workspace note"
        );
    }
}

#[test]
fn agents_export_refuses_unresolvable_destination_parents_before_writing() {
    let install = tempfile::tempdir().unwrap();
    write_config(install.path(), None);
    write(
        &install
            .path()
            .join("agents/export_fixture/workspace/notes.md"),
        "workspace note",
    );
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("bundle");
    let unresolvable = parent.path().join("missing/../bundle");

    let output = export(install.path(), &unresolvable, false);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("before `..`"));
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);

    write(&out.join("keep.txt"), "previous bundle");
    let output = export(install.path(), &unresolvable, true);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("before `..`"));
    assert_eq!(
        std::fs::read_to_string(out.join("keep.txt")).unwrap(),
        "previous bundle"
    );
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
}

#[test]
fn agents_export_accepts_existing_parent_traversal_in_source_and_destination() {
    let install = tempfile::tempdir().unwrap();
    std::fs::create_dir(install.path().join("existing")).unwrap();
    write_config(install.path(), Some(Path::new("existing/../workspace")));
    write(&install.path().join("workspace/notes.md"), "workspace note");

    let output = export(install.path(), Path::new("existing/../bundle"), false);
    assert_success(&output);
    assert_eq!(
        std::fs::read_to_string(install.path().join("bundle/workspace/notes.md")).unwrap(),
        "workspace note"
    );
    assert_eq!(
        std::fs::read_dir(install.path().join("existing"))
            .unwrap()
            .count(),
        0
    );
}
