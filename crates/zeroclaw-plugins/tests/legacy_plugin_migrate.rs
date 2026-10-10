//! Supported recovery relocates bytes without changing admission or authority.

use zeroclaw_config::schema::v2::workspace_toplevel_v3_path;
use zeroclaw_config::schema::{Config, PluginsConfig, legacy_plugin_dirs_with_entries};
use zeroclaw_plugins::host::{PluginHost, migrate_plugins_dir};

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
    let source = workspace_toplevel_v3_path(&install, "plugins");
    package(&source, "alpha", "recoverable-bytes");
    package(&source, "beta", "legacy-beta");
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
