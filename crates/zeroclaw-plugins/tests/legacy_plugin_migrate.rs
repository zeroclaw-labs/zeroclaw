//! Supported recovery relocates bytes without changing admission or authority.

use zeroclaw_config::schema::v2::{
    migrate_v2_to_v3_install_filesystem, workspace_toplevel_v3_path,
};
use zeroclaw_config::schema::{Config, PluginsConfig, legacy_plugin_dirs_with_entries};
use zeroclaw_plugins::error::PluginError;
use zeroclaw_plugins::host::{PluginHost, migrate_plugins_dir};
use zeroclaw_plugins::signature::SignatureMode;

fn package(root: &std::path::Path, name: &str, payload: &str) {
    std::fs::create_dir_all(root.join(name)).unwrap();
    std::fs::write(
        root.join(name).join("manifest.toml"),
        format!("name = \"{name}\"\n"),
    )
    .unwrap();
    std::fs::write(root.join(name).join("payload"), payload).unwrap();
}

#[test]
fn migrated_workspace_packages_reach_recovery_without_clobber_or_admission() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("current-plugins");
    let config = Config {
        config_path: install.join("config.toml"),
        data_dir: install.join("data"),
        plugins: PluginsConfig {
            plugins_dir: target.to_string_lossy().into_owned(),
            ..Default::default()
        },
        ..Default::default()
    };
    let authority = serde_json::to_value(&config.plugins).unwrap();
    let source = install.join("data/plugins");
    let legacy = install.join("workspace/plugins");
    package(&legacy, "alpha", "recoverable-bytes");
    package(&legacy, "beta", "legacy-beta");
    let report = migrate_v2_to_v3_install_filesystem(&install).unwrap();
    assert!(report.entries_relocated > 0);
    let backup = report.backup_dir.unwrap().join("legacy-workspace/plugins");
    assert_eq!(
        std::fs::read(backup.join("alpha/payload")).unwrap(),
        b"recoverable-bytes"
    );
    assert_eq!(
        std::fs::read(source.join("alpha/payload")).unwrap(),
        b"recoverable-bytes"
    );
    assert_eq!(workspace_toplevel_v3_path(&install, "plugins"), source);
    assert!(!install.join("agents/default/workspace/plugins").exists());
    package(&target, "beta", "operator-beta");
    let recovery = legacy_plugin_dirs_with_entries(&config);
    assert_eq!(recovery, vec![source.clone()]);
    let moved: usize = recovery
        .iter()
        .map(|dir| migrate_plugins_dir(dir, &target).unwrap())
        .sum();
    assert_eq!(moved, 1);
    assert_eq!(
        std::fs::read(target.join("alpha/payload")).unwrap(),
        b"recoverable-bytes"
    );
    assert_eq!(
        std::fs::read(target.join("beta/payload")).unwrap(),
        b"operator-beta"
    );
    assert_eq!(
        std::fs::read(source.join("beta/payload")).unwrap(),
        b"legacy-beta"
    );
    assert!(!source.join("alpha").exists());
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    // Recovery finds and relocates directories. These deliberately incomplete
    // manifests still fail the existing host parser, even with signing disabled.
    assert!(
        PluginHost::from_plugins_dir(&target)
            .unwrap()
            .list_plugins()
            .is_empty()
    );
    assert_eq!(migrate_plugins_dir(&source, &target).unwrap(), 0);
}

fn historical_config(install: &std::path::Path, target: &std::path::Path) -> Config {
    Config {
        config_path: install.join("config.toml"),
        data_dir: install.join("data"),
        plugins: PluginsConfig {
            plugins_dir: target.to_string_lossy().into_owned(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn skill_package(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("skills/fixture")).unwrap();
    std::fs::write(
        dir.join("manifest.toml"),
        format!("name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("skills/fixture/SKILL.md"),
        "---\nname: fixture\ndescription: Selected fixture skill\n---\n# Selected skill\n",
    )
    .unwrap();
    dir
}

/// Constructed prior-layout contents are not automatically selected, even
/// when a compatible package appears after the resolver ran.
#[test]
fn historical_agent_workspace_is_not_a_directory_wide_migration_source() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let config = historical_config(&install, &target);
    let live = config.agent_workspace_dir("default").join("plugins");
    let selected = skill_package(&live, "historical");
    let before = std::fs::read(selected.join("manifest.toml")).unwrap();
    let authority = serde_json::to_value(&config.plugins).unwrap();
    let recovery = legacy_plugin_dirs_with_entries(&config);
    let inserted = skill_package(&live, "inserted");
    let moved: usize = recovery
        .iter()
        .map(|source| migrate_plugins_dir(source, &target).unwrap())
        .sum();
    assert!(recovery.is_empty());
    assert_eq!(moved, 0);
    assert!(!target.join("historical").exists());
    assert!(!target.join("inserted").exists());
    assert_eq!(
        std::fs::read(selected.join("manifest.toml")).unwrap(),
        before
    );
    assert!(inserted.join("skills/fixture/SKILL.md").is_file());
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
}

/// This reaches the host admission/install boundary used by the existing CLI.
/// It does not exercise the CLI's WASM load gate or config/grant seeding.
#[test]
fn explicit_historical_install_adopts_only_the_selected_compatible_package() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let config = historical_config(&install, &target);
    let live = config.agent_workspace_dir("default").join("plugins");
    let selected = skill_package(&live, "selected");
    let sibling = skill_package(&live, "unselected");
    let source_manifest = std::fs::read(selected.join("manifest.toml")).unwrap();
    let source_skill = std::fs::read(selected.join("skills/fixture/SKILL.md")).unwrap();
    assert!(legacy_plugin_dirs_with_entries(&config).is_empty());

    let mut host = PluginHost::from_plugins_dir_with_security(
        &config.plugins.resolved_plugins_dir(),
        PluginHost::resolve_signature_mode(&config.plugins.security.signature_mode),
        config.plugins.security.trusted_publisher_keys.clone(),
    )
    .unwrap();
    let admitted = host.admit_source(selected.to_str().unwrap()).unwrap();
    assert!(admitted.component().is_none());
    assert_eq!(host.install_admitted(admitted).unwrap(), "selected");
    assert_eq!(
        std::fs::read(target.join("selected/manifest.toml")).unwrap(),
        source_manifest
    );
    assert_eq!(
        std::fs::read(target.join("selected/skills/fixture/SKILL.md")).unwrap(),
        source_skill
    );
    assert_eq!(
        std::fs::read(selected.join("manifest.toml")).unwrap(),
        source_manifest
    );
    assert_eq!(
        std::fs::read(selected.join("skills/fixture/SKILL.md")).unwrap(),
        source_skill
    );
    assert!(sibling.join("manifest.toml").is_file());
    assert!(!target.join("unselected").exists());
    assert!(matches!(
        host.admit_source(selected.to_str().unwrap()),
        Err(PluginError::AlreadyLoaded(_))
    ));
    assert_eq!(
        std::fs::read(target.join("selected/manifest.toml")).unwrap(),
        source_manifest
    );

    package(&live, "incomplete", "unsupported-legacy-payload");
    assert!(
        host.admit_source(live.join("incomplete").to_str().unwrap())
            .is_err()
    );
    assert!(!target.join("incomplete").exists());
    assert_eq!(
        std::fs::read(live.join("incomplete/payload")).unwrap(),
        b"unsupported-legacy-payload"
    );
}

#[test]
fn explicit_historical_install_preserves_strict_signature_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let mut config = historical_config(&install, &target);
    config.plugins.security.signature_mode = "strict".into();
    let source = skill_package(
        &config.agent_workspace_dir("default").join("plugins"),
        "unsigned",
    );
    let before = std::fs::read(source.join("manifest.toml")).unwrap();
    let mode = PluginHost::resolve_signature_mode(&config.plugins.security.signature_mode);
    assert!(matches!(mode, SignatureMode::Strict));
    let host = PluginHost::from_plugins_dir_with_security(
        &config.plugins.resolved_plugins_dir(),
        mode,
        config.plugins.security.trusted_publisher_keys.clone(),
    )
    .unwrap();
    assert!(matches!(
        host.admit_source(source.to_str().unwrap()),
        Err(PluginError::UnsignedPlugin(_))
    ));
    assert!(host.list_plugins().is_empty());
    assert!(!target.join("unsigned").exists());
    assert_eq!(std::fs::read(source.join("manifest.toml")).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn linked_producer_root_recovers_bytes_without_clobber_or_config_changes() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let mut config = historical_config(&install, &target);
    // A sibling with a matching string prefix is not workspace containment.
    let workspace = tmp.path().join("workspace");
    config
        .agents
        .entry("worker".into())
        .or_default()
        .workspace
        .path = Some(workspace.clone());
    let legacy = install.join("workspace/plugins");
    let alpha = skill_package(&legacy, "alpha");
    let beta = skill_package(&legacy, "beta");
    std::fs::write(alpha.join("payload"), b"alpha-legacy").unwrap();
    std::fs::write(beta.join("payload"), b"beta-legacy").unwrap();
    let report = migrate_v2_to_v3_install_filesystem(&install).unwrap();
    assert!(report.entries_relocated > 0);
    assert_eq!(
        std::fs::read(
            report
                .backup_dir
                .unwrap()
                .join("legacy-workspace/plugins/alpha/payload")
        )
        .unwrap(),
        b"alpha-legacy"
    );
    let produced = install.join("data/plugins");
    assert_eq!(
        std::fs::read(produced.join("alpha/payload")).unwrap(),
        b"alpha-legacy"
    );
    // Arrange a supported linked-root layout using actual producer bytes.
    let external = tmp.path().join("workspace-sibling");
    std::fs::rename(&produced, &external).unwrap();
    symlink(&external, &produced).unwrap();
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    symlink(&external, &legacy).unwrap();
    let current_beta = skill_package(&target, "beta");
    std::fs::write(current_beta.join("payload"), b"beta-current").unwrap();
    let authority = serde_json::to_value(&config.plugins).unwrap();
    let recovery = legacy_plugin_dirs_with_entries(&config);
    assert_eq!(recovery, vec![produced.clone()]);
    let moved: usize = recovery
        .iter()
        .map(|source| migrate_plugins_dir(source, &target).unwrap())
        .sum();
    assert_eq!(moved, 1);
    assert_eq!(
        std::fs::read(target.join("alpha/payload")).unwrap(),
        b"alpha-legacy"
    );
    assert_eq!(
        std::fs::read(target.join("beta/payload")).unwrap(),
        b"beta-current"
    );
    assert_eq!(
        std::fs::read(external.join("beta/payload")).unwrap(),
        b"beta-legacy"
    );
    assert!(!external.join("alpha").exists());
    assert!(std::fs::symlink_metadata(&produced).unwrap().is_symlink());
    assert!(std::fs::symlink_metadata(&legacy).unwrap().is_symlink());
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    assert_eq!(migrate_plugins_dir(&produced, &target).unwrap(), 0);
    assert!(
        !workspace.exists(),
        "selection must not create an absent workspace"
    );

    let mut same_target = config.clone();
    same_target.plugins.plugins_dir = legacy.to_string_lossy().into_owned();
    assert!(legacy_plugin_dirs_with_entries(&same_target).is_empty());
    assert_eq!(
        std::fs::read(external.join("beta/payload")).unwrap(),
        b"beta-legacy"
    );
}

#[cfg(unix)]
#[test]
fn aliases_into_known_agent_workspaces_are_not_automatically_promoted() {
    use std::os::unix::fs::symlink;

    for kind in ["default", "historical", "configured"] {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("install");
        let target = tmp.path().join("managed");
        let mut config = historical_config(&install, &target);
        let workspace = match kind {
            "historical" => {
                config
                    .agents
                    .entry("default".into())
                    .or_default()
                    .workspace
                    .path = Some(tmp.path().join("new-default"));
                install.join("agents/default/workspace")
            }
            "configured" => {
                let agent = config.agents.entry("worker".into()).or_default();
                agent.enabled = false;
                agent.workspace.path = Some(tmp.path().join("custom-workspace"));
                config.agent_workspace_dir("worker")
            }
            _ => config.agent_workspace_dir("default"),
        };
        let packages = workspace.join("plugins");
        let package = skill_package(&packages, "ordinary");
        let before = std::fs::read(package.join("manifest.toml")).unwrap();
        let skill = std::fs::read(package.join("skills/fixture/SKILL.md")).unwrap();
        std::fs::create_dir_all(&config.data_dir).unwrap();
        symlink(&packages, config.data_dir.join("plugins")).unwrap();
        let authority = serde_json::to_value(&config.plugins).unwrap();
        let recovery = legacy_plugin_dirs_with_entries(&config);
        let moved: usize = recovery
            .iter()
            .map(|source| migrate_plugins_dir(source, &target).unwrap())
            .sum();
        assert!(recovery.is_empty(), "{kind}: workspace alias selected");
        assert_eq!(moved, 0);
        assert!(!target.join("ordinary").exists());
        assert_eq!(
            std::fs::read(package.join("manifest.toml")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(package.join("skills/fixture/SKILL.md")).unwrap(),
            skill
        );
        assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    }
}

#[test]
fn roots_containing_known_or_absent_agent_workspaces_are_not_promoted() {
    for present in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("install");
        let target = tmp.path().join("managed");
        let mut config = historical_config(&install, &target);
        let candidate = config.data_dir.join("plugins");
        let package = skill_package(&candidate, "ordinary");
        let workspace = if present {
            skill_package(&candidate, "agent-package")
        } else {
            candidate
                .join("not-created")
                .join("..")
                .join("future-workspace")
        };
        config
            .agents
            .entry("worker".into())
            .or_default()
            .workspace
            .path = Some(workspace.clone());
        let before = std::fs::read(package.join("manifest.toml")).unwrap();
        let skill = std::fs::read(package.join("skills/fixture/SKILL.md")).unwrap();
        let authority = serde_json::to_value(&config.plugins).unwrap();
        let recovery = legacy_plugin_dirs_with_entries(&config);
        let moved: usize = recovery
            .iter()
            .map(|source| migrate_plugins_dir(source, &target).unwrap())
            .sum();
        assert!(
            recovery.is_empty(),
            "an ancestor of a known workspace was selected"
        );
        assert_eq!(moved, 0);
        assert!(!target.join("ordinary").exists());
        assert!(!target.join("agent-package").exists());
        assert_eq!(
            std::fs::read(package.join("manifest.toml")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(package.join("skills/fixture/SKILL.md")).unwrap(),
            skill
        );
        if present {
            assert!(workspace.join("manifest.toml").is_file());
        } else {
            assert!(!candidate.join("not-created").exists());
            assert!(!candidate.join("future-workspace").exists());
        }
        assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    }
}

#[cfg(unix)]
#[test]
fn absent_workspace_under_existing_parent_alias_is_not_promoted() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let mut config = historical_config(&install, &target);
    let candidate = config.data_dir.join("plugins");
    let package = skill_package(&candidate, "ordinary");
    let parent_alias = tmp.path().join("workspace-parent-alias");
    std::os::unix::fs::symlink(&candidate, &parent_alias).unwrap();
    let workspace = parent_alias.join("not-created");
    config
        .agents
        .entry("worker".into())
        .or_default()
        .workspace
        .path = Some(workspace.clone());
    let before = std::fs::read(package.join("manifest.toml")).unwrap();
    let authority = serde_json::to_value(&config.plugins).unwrap();
    let recovery = legacy_plugin_dirs_with_entries(&config);
    let moved: usize = recovery
        .iter()
        .map(|source| migrate_plugins_dir(source, &target).unwrap())
        .sum();
    assert!(recovery.is_empty());
    assert_eq!(moved, 0);
    assert!(!target.join("ordinary").exists());
    assert_eq!(
        std::fs::read(package.join("manifest.toml")).unwrap(),
        before
    );
    assert!(!workspace.exists());
    assert!(!candidate.join("not-created").exists());
    assert!(
        std::fs::symlink_metadata(parent_alias)
            .unwrap()
            .is_symlink()
    );
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
}

#[cfg(unix)]
fn skill_bytes(dir: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    (
        std::fs::read(dir.join("manifest.toml")).unwrap(),
        std::fs::read(dir.join("skills/fixture/SKILL.md")).unwrap(),
    )
}

#[cfg(unix)]
#[test]
fn linked_child_package_is_left_at_source_without_a_recovery_hint() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let config = historical_config(&install, &target);
    let source = config.data_dir.join("plugins");
    let canonical = skill_package(&tmp.path().join("external"), "linked");
    let before = skill_bytes(&canonical);
    let authority = serde_json::to_value(&config.plugins).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    let link = source.join("linked");
    symlink(&canonical, &link).unwrap();

    assert!(legacy_plugin_dirs_with_entries(&config).is_empty());
    // Call the actual mover independently of the hint: mixed roots also reach it.
    assert_eq!(migrate_plugins_dir(&source, &target).unwrap(), 0);
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), canonical);
    assert_eq!(skill_bytes(&canonical), before);
    assert!(!target.exists());
    assert!(
        PluginHost::from_plugins_dir(&source)
            .unwrap()
            .list_plugins()
            .is_empty()
    );
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    assert_eq!(migrate_plugins_dir(&source, &target).unwrap(), 0);
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn mixed_root_alias_moves_only_ordinary_packages_and_preserves_retry() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let config = historical_config(&install, &target);
    let source = tmp.path().join("external-legacy-root");
    let regular = skill_package(&source, "regular");
    let regular_before = skill_bytes(&regular);
    let collision = skill_package(&source, "collision");
    std::fs::write(collision.join("payload"), b"legacy-collision").unwrap();
    let collision_before = skill_bytes(&collision);
    let current = skill_package(&target, "collision");
    std::fs::write(current.join("payload"), b"current-collision").unwrap();
    let current_before = skill_bytes(&current);
    let canonical = skill_package(&tmp.path().join("external-package"), "linked");
    let linked_before = skill_bytes(&canonical);
    let child_link = source.join("linked");
    symlink(&canonical, &child_link).unwrap();
    std::fs::create_dir_all(&config.data_dir).unwrap();
    let root_alias = config.data_dir.join("plugins");
    symlink(&source, &root_alias).unwrap();
    let authority = serde_json::to_value(&config.plugins).unwrap();
    let resolved_target = config.plugins.resolved_plugins_dir();
    assert_eq!(resolved_target, target);

    let recovery = legacy_plugin_dirs_with_entries(&config);
    assert_eq!(recovery, vec![root_alias.clone()]);
    let moved: usize = recovery
        .iter()
        .map(|dir| migrate_plugins_dir(dir, &resolved_target).unwrap())
        .sum();
    assert_eq!(moved, 1);
    assert_eq!(skill_bytes(&target.join("regular")), regular_before);
    assert!(!regular.exists());
    assert_eq!(skill_bytes(&collision), collision_before);
    assert_eq!(skill_bytes(&current), current_before);
    assert_eq!(
        std::fs::read(collision.join("payload")).unwrap(),
        b"legacy-collision"
    );
    assert_eq!(
        std::fs::read(current.join("payload")).unwrap(),
        b"current-collision"
    );
    assert!(std::fs::symlink_metadata(&child_link).unwrap().is_symlink());
    assert_eq!(std::fs::read_link(&child_link).unwrap(), canonical);
    assert_eq!(skill_bytes(&canonical), linked_before);
    assert!(!target.join("linked").exists());
    assert_eq!(std::fs::read_link(&root_alias).unwrap(), source);
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    let names: Vec<_> = PluginHost::from_plugins_dir(&resolved_target)
        .unwrap()
        .list_plugins()
        .into_iter()
        .map(|plugin| plugin.name)
        .collect();
    assert_eq!(names, vec!["collision", "regular"]);
    assert_eq!(
        migrate_plugins_dir(&root_alias, &resolved_target).unwrap(),
        0
    );
    assert_eq!(legacy_plugin_dirs_with_entries(&config), vec![root_alias]);
    assert_eq!(skill_bytes(&canonical), linked_before);
    assert!(std::fs::symlink_metadata(&child_link).unwrap().is_symlink());
    assert!(!target.join("linked").exists());
}

#[cfg(unix)]
#[test]
fn ordinary_child_with_manifest_alias_remains_discoverable_after_migration() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let target = tmp.path().join("managed");
    let config = historical_config(&install, &target);
    let source = config.data_dir.join("plugins");
    let package = skill_package(&source, "manifest-alias");
    let canonical_manifest = tmp.path().join("shared-manifest.toml");
    std::fs::rename(package.join("manifest.toml"), &canonical_manifest).unwrap();
    symlink(&canonical_manifest, package.join("manifest.toml")).unwrap();
    let before = skill_bytes(&package);
    let authority = serde_json::to_value(&config.plugins).unwrap();
    assert_eq!(
        legacy_plugin_dirs_with_entries(&config),
        vec![source.clone()]
    );
    assert_eq!(migrate_plugins_dir(&source, &target).unwrap(), 1);
    let moved = target.join("manifest-alias");
    assert_eq!(skill_bytes(&moved), before);
    assert_eq!(
        std::fs::read_link(moved.join("manifest.toml")).unwrap(),
        canonical_manifest
    );
    assert!(!package.exists());
    let names: Vec<_> = PluginHost::from_plugins_dir(&config.plugins.resolved_plugins_dir())
        .unwrap()
        .list_plugins()
        .into_iter()
        .map(|plugin| plugin.name)
        .collect();
    assert_eq!(names, vec!["manifest-alias"]);
    assert_eq!(serde_json::to_value(&config.plugins).unwrap(), authority);
    assert_eq!(migrate_plugins_dir(&source, &target).unwrap(), 0);
    assert!(legacy_plugin_dirs_with_entries(&config).is_empty());
    assert_eq!(std::fs::read(&canonical_manifest).unwrap(), before.0);
}
