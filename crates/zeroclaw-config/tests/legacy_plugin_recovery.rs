//! Future splits keep host-data recovery reachable without trusting live workspaces.

use std::ffi::OsString;
use std::path::Path;
use zeroclaw_config::migration::CURRENT_SCHEMA_VERSION;
use zeroclaw_config::schema::v2::{
    migrate_v2_to_v3_install_filesystem, workspace_toplevel_v3_path,
};
use zeroclaw_config::schema::{Config, legacy_plugin_dirs_with_entries, resolve_runtime_dirs};

struct EnvGuard(Vec<(&'static str, Option<OsString>)>);

impl EnvGuard {
    fn isolated(home: &Path, config: Option<&Path>, data: Option<&Path>) -> Self {
        let mut guard = Self(Vec::new());
        for (key, value) in [
            ("HOME", Some(home)),
            ("ZEROCLAW_CONFIG_DIR", config),
            ("ZEROCLAW_DATA_DIR", data),
            ("ZEROCLAW_WORKSPACE", None),
        ] {
            guard.0.push((key, std::env::var_os(key)));
            // SAFETY: this integration binary has one current-thread test;
            // no concurrent test or background task accesses these variables.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        guard
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..).rev() {
            // SAFETY: the sole test has awaited its loader before restoration.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn seed_database(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE marker(value TEXT); INSERT INTO marker VALUES('retained-row');",
        )
        .unwrap();
}

fn marker(path: &Path) -> String {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("SELECT value FROM marker", [], |row| row.get(0))
        .unwrap()
}

fn package(dir: &Path, payload: &str) {
    std::fs::create_dir_all(dir.join("synthetic-package")).unwrap();
    std::fs::write(
        dir.join("synthetic-package/manifest.toml"),
        "name = \"synthetic-package\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("synthetic-package/payload"), payload).unwrap();
}

/// Live workspace and forged metadata are not automatic recovery authority.
/// Keep these in the sole loader test so its global environment stays serialized.
fn live_workspace_and_lookalike_metadata_do_not_authorize_recovery() {
    let root = tempfile::tempdir().unwrap();
    let install = root.path().join("install");
    let config = Config {
        config_path: install.join("config.toml"),
        data_dir: install.join("data"),
        plugins: zeroclaw_config::schema::PluginsConfig {
            plugins_dir: root
                .path()
                .join("managed-plugins")
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        },
        ..Default::default()
    };
    let live = config.agent_workspace_dir("default").join("plugins");
    assert_ne!(live, workspace_toplevel_v3_path(&install, "plugins"));
    // Constructed historical/collision layout, not output from the new producer.
    let unsplit = install.join("workspace/plugins");
    package(&unsplit, "unmoved-legacy-bytes");
    package(&live, "ordinary-live-workspace-bytes");
    std::fs::write(
        live.join("synthetic-package/manifest.toml"),
        "name = \"synthetic-package\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n",
    )
    .unwrap();
    assert!(
        !legacy_plugin_dirs_with_entries(&config).contains(&live),
        "an ordinary package in the live agent workspace is not migration provenance"
    );

    // The real producer does not create this receipt. The agent can fabricate
    // both it and a backup-shaped subtree within its own writable workspace.
    let workspace = config.agent_workspace_dir("default");
    let lookalike = workspace.join("backup-20000101T000000/legacy-workspace/plugins");
    package(&lookalike, "ordinary-live-workspace-bytes");
    std::fs::copy(
        live.join("synthetic-package/manifest.toml"),
        lookalike.join("synthetic-package/manifest.toml"),
    )
    .unwrap();
    std::fs::write(
        live.join(".v2-workspace-split-receipt.json"),
        serde_json::to_vec(&serde_json::json!({
            "producer": "zeroclaw",
            "schema_version": CURRENT_SCHEMA_VERSION,
            "moved_packages": ["synthetic-package"],
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        legacy_plugin_dirs_with_entries(&config),
        vec![unsplit.clone()],
        "matching names and forged metadata do not authorize the live collision"
    );
    assert_eq!(
        std::fs::read(unsplit.join("synthetic-package/payload")).unwrap(),
        b"unmoved-legacy-bytes"
    );
    assert_eq!(
        std::fs::read(live.join("synthetic-package/payload")).unwrap(),
        b"ordinary-live-workspace-bytes"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn loader_keeps_legacy_plugin_recovery_in_each_install_layout() {
    let mut checked = 0;
    for (layout, state) in [
        ("explicit", "partial"),
        ("default", "partial"),
        ("container", "partial"),
        ("explicit", "resumed"),
        ("default", "resumed"),
        ("container", "resumed"),
        ("explicit", "collision"),
        ("explicit", "configured-target"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let install = if layout == "explicit" {
            root.path().join("missing/parent/install")
        } else {
            home.join(".zeroclaw")
        };
        let old = install.join("workspace/plugins");
        // Independent expected destination, not derived from the producer mapping.
        let migrated = install.join("data/plugins");
        let resumed = state == "resumed";
        std::fs::create_dir_all(install.join("agents/default/workspace")).unwrap();
        std::fs::write(
            install.join("agents/default/workspace/IDENTITY.md"),
            "already-moved",
        )
        .unwrap();
        package(&old, "retained-package-bytes");
        if resumed {
            // The actual new producer creates this future resumed DataDir
            // layout; this is not a constructed historical agent-workspace fixture.
            let report = migrate_v2_to_v3_install_filesystem(&install).unwrap();
            assert!(report.entries_relocated > 0);
            let backup = report.backup_dir.unwrap().join("legacy-workspace");
            assert_eq!(
                std::fs::read(backup.join("plugins/synthetic-package/payload")).unwrap(),
                b"retained-package-bytes"
            );
        }
        if state == "collision" {
            package(&migrated, "operator-package-bytes");
        }
        let configured = if state == "configured-target" {
            migrated.clone()
        } else {
            root.path().join("explicit-current-plugins")
        };
        let schema = if resumed { CURRENT_SCHEMA_VERSION } else { 2 };
        std::fs::write(
            install.join("config.toml"),
            format!(
                "schema_version = {schema}\n[plugins]\nplugins_dir = {}\n",
                toml::Value::String(configured.to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        let container_data = home.join("data");
        assert_ne!(container_data, install.join("data"));
        let _env = EnvGuard::isolated(
            &home,
            (layout == "explicit").then_some(install.as_path()),
            (layout == "container").then_some(container_data.as_path()),
        );
        let (_, prelock) = resolve_runtime_dirs().await.unwrap();
        std::fs::create_dir_all(&prelock).unwrap();
        let database_root = if resumed {
            prelock.clone()
        } else {
            install.join("workspace")
        };
        seed_database(&database_root.join("devices.db"));
        seed_database(&database_root.join("memory/brain.db"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock_path = prelock.join(".lifecycle/config-lifecycle.lock");
        std::fs::create_dir_all(prelock.join(".lifecycle")).unwrap();
        let owner = options.open(&lock_path).unwrap();
        owner.try_lock().unwrap();
        let rival = options.open(&lock_path).unwrap();
        assert!(rival.try_lock().is_err(), "concurrent owner admitted");
        let loaded = Box::pin(Config::load_or_init()).await.unwrap();
        assert_eq!(
            loaded.data_dir, prelock,
            "{layout}/{state}: loader/lock disagree"
        );
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.plugins.resolved_plugins_dir(), configured);
        assert_eq!(marker(&loaded.data_dir.join("devices.db")), "retained-row");
        assert_eq!(
            marker(&loaded.data_dir.join("memory/brain.db")),
            "retained-row"
        );
        let expected = if state == "collision" {
            &old
        } else {
            &migrated
        };
        assert_eq!(
            std::fs::read(expected.join("synthetic-package/payload")).unwrap(),
            b"retained-package-bytes"
        );
        assert_eq!(workspace_toplevel_v3_path(&install, "plugins"), migrated);
        assert!(!install.join("agents/default/workspace/plugins").exists());
        let recovery = legacy_plugin_dirs_with_entries(&loaded);
        if layout == "container" {
            // Retain the real loader/lock path above. Also exercise its loaded
            // install with a distinct public Config data override: the producer
            // destination must remain reachable independently of that field.
            let mut hint_config = loaded.clone();
            hint_config.data_dir = container_data.clone();
            assert_ne!(hint_config.data_dir.join("plugins"), migrated);
            assert!(legacy_plugin_dirs_with_entries(&hint_config).contains(&migrated));
            #[cfg(unix)]
            {
                // The actual producer/loader bytes remain recoverable through
                // a supported data-root alias, deduplicated with the producer path.
                let alias = hint_config.data_dir.join("plugins");
                std::fs::create_dir_all(&hint_config.data_dir).unwrap();
                std::os::unix::fs::symlink(&migrated, &alias).unwrap();
                assert_eq!(
                    legacy_plugin_dirs_with_entries(&hint_config),
                    vec![alias.clone()]
                );
                assert_eq!(
                    std::fs::read(alias.join("synthetic-package/payload")).unwrap(),
                    b"retained-package-bytes"
                );
            }
        }
        if state == "configured-target" {
            assert!(
                recovery.is_empty(),
                "configured live root must not be migrated again"
            );
        } else {
            assert!(
                recovery.contains(expected),
                "{layout}/{state}: recovery missing"
            );
            if state == "collision" {
                assert!(
                    recovery.contains(&migrated),
                    "existing host-data bytes remain a legacy source without clobbering"
                );
                assert_eq!(
                    std::fs::read(migrated.join("synthetic-package/payload")).unwrap(),
                    b"operator-package-bytes"
                );
            }
        }
        if !resumed {
            let backup = std::fs::read_dir(&install)
                .unwrap()
                .flatten()
                .find(|entry| entry.file_name().to_string_lossy().starts_with("backup-"))
                .unwrap()
                .path()
                .join("legacy-workspace");
            assert_eq!(marker(&backup.join("devices.db")), "retained-row");
            assert_eq!(
                std::fs::read(backup.join("plugins/synthetic-package/payload")).unwrap(),
                b"retained-package-bytes"
            );
        }
        drop(owner);
        rival.try_lock().unwrap();
        checked += 1;
        println!(
            "PASS {layout}/{state}: recovery={}, schema{CURRENT_SCHEMA_VERSION}/SQLite/backup/lock preserved",
            recovery.len()
        );
    }
    assert_eq!(checked, 8);
    live_workspace_and_lookalike_metadata_do_not_authorize_recovery();
}
