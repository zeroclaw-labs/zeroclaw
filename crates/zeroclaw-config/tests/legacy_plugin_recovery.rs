//! The loader must leave legacy package recovery reachable after a resumed split.

use std::ffi::OsString;
use std::path::Path;
use zeroclaw_config::schema::v2::workspace_toplevel_v3_path;
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
        let migrated = workspace_toplevel_v3_path(&install, "plugins");
        let resumed = state == "resumed";
        std::fs::create_dir_all(install.join("agents/default/workspace")).unwrap();
        std::fs::write(
            install.join("agents/default/workspace/IDENTITY.md"),
            "already-moved",
        )
        .unwrap();
        package(
            if resumed { &migrated } else { &old },
            "retained-package-bytes",
        );
        if state == "collision" {
            package(&migrated, "operator-package-bytes");
        }
        let configured = if state == "configured-target" {
            migrated.clone()
        } else {
            root.path().join("explicit-current-plugins")
        };
        let schema = if resumed { 3 } else { 2 };
        std::fs::write(
            install.join("config.toml"),
            format!(
                "schema_version = {schema}\n[plugins]\nplugins_dir = {}\n",
                toml::Value::String(configured.to_string_lossy().into_owned())
            ),
        )
        .unwrap();
        let container_data = home.join("data");
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
        let owner = options.open(prelock.join("config-lifecycle.lock")).unwrap();
        owner.try_lock().unwrap();
        let rival = options.open(prelock.join("config-lifecycle.lock")).unwrap();
        assert!(rival.try_lock().is_err(), "concurrent owner admitted");
        let loaded = Box::pin(Config::load_or_init()).await.unwrap();
        assert_eq!(
            loaded.data_dir, prelock,
            "{layout}/{state}: loader/lock disagree"
        );
        assert_eq!(loaded.schema_version, 3);
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
        let recovery = legacy_plugin_dirs_with_entries(&loaded);
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
                assert!(recovery.contains(&migrated));
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
            "PASS {layout}/{state}: recovery={}, schema3/SQLite/backup/lock preserved",
            recovery.len()
        );
    }
    assert_eq!(checked, 8);
}
