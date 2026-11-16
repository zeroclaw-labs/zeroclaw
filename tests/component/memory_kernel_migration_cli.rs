//! Offline recovery must dispatch before normal configuration and proxy boot.

use std::path::Path;
use std::process::{Command, Output};

fn migrate(config_dir: &Path, source: &Path, destination: &Path, store: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .arg("--config-dir")
        .arg(config_dir)
        .args(["memory", "migrate-kernel", "--source"])
        .arg(source)
        .arg("--destination")
        .arg(destination)
        .args(["--store", store, "--approved-by", "component-fixture"])
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("LC_ALL", "C")
        .output()
        .expect("run the offline migration command")
}

fn assert_migration_error(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "missing source must be refused");
    assert!(
        output.stdout.is_empty(),
        "failed migration must not emit a receipt"
    );
    #[cfg(not(feature = "memory-tachi"))]
    assert!(
        stderr.contains("memory-tachi"),
        "unexpected error: {stderr}"
    );
    #[cfg(all(feature = "memory-tachi", unix))]
    assert!(
        stderr.to_lowercase().contains("source"),
        "unexpected error: {stderr}"
    );
    #[cfg(all(feature = "memory-tachi", not(unix)))]
    assert!(stderr.contains("Unix"), "unexpected error: {stderr}");
}

#[test]
fn kernel_migration_does_not_initialize_a_missing_config_directory() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("not-configured");
    let source = root.path().join("missing-source.db");
    let destination = root.path().join("new.db");
    for store in ["memory", "companion"] {
        let output = migrate(&config_dir, &source, &destination, store);
        assert_migration_error(&output);
        assert!(!config_dir.exists());
        assert!(!source.exists());
        assert!(!destination.exists());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn kernel_migration_bypasses_malformed_agent_config() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("config");
    std::fs::create_dir(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    let original = b"[proxy\nnot valid TOML, intentionally\n";
    std::fs::write(&config_path, original).unwrap();
    let source = root.path().join("missing-source.db");
    let destination = root.path().join("new.db");
    for store in ["memory", "companion"] {
        let output = migrate(&config_dir, &source, &destination, store);
        assert_migration_error(&output);
        assert_eq!(std::fs::read(&config_path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&config_dir).unwrap().count(), 1);
        assert!(!destination.exists());
    }
}

fn migrate_native(config_dir: &Path, source: &Path, destination: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .arg("--config-dir")
        .arg(config_dir)
        .args(["memory", "migrate-native", "--source"])
        .arg(source)
        .arg("--destination")
        .arg(destination)
        .args(["--approved-by", "component-fixture"])
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("LC_ALL", "C")
        .output()
        .expect("run the offline native migration command")
}

#[test]
fn native_migration_does_not_initialize_a_missing_config_directory() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("not-configured");
    let source = root.path().join("missing-source.db");
    let destination = root.path().join("tachi.db");
    let output = migrate_native(&config_dir, &source, &destination);
    assert_migration_error(&output);
    assert!(!config_dir.exists());
    assert!(!source.exists());
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn native_migration_bypasses_malformed_agent_config() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("config");
    std::fs::create_dir(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    let original = b"[proxy\nnot valid TOML, intentionally\n";
    std::fs::write(&config_path, original).unwrap();
    let source = root.path().join("missing-source.db");
    let destination = root.path().join("tachi.db");
    let output = migrate_native(&config_dir, &source, &destination);
    assert_migration_error(&output);
    assert_eq!(std::fs::read(&config_path).unwrap(), original);
    assert_eq!(std::fs::read_dir(&config_dir).unwrap().count(), 1);
    assert!(!destination.exists());
}

#[cfg(all(feature = "memory-tachi", feature = "agent-runtime", unix))]
#[test]
fn native_migration_converts_real_source_without_config() {
    use zeroclaw_memory::traits::{Memory, MemoryKind, SemanticSubtype};

    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("not-configured");
    let src_ws = root.path().join("src-workspace");
    let dst_ws = root.path().join("dst-workspace");
    std::fs::create_dir_all(src_ws.join("memory")).unwrap();
    std::fs::create_dir_all(dst_ws.join("memory")).unwrap();

    let source = src_ws.join("memory").join("brain.db");
    let destination = dst_ws.join("memory").join("tachi.db");

    {
        let conn = rusqlite::Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE agents (
                 id TEXT PRIMARY KEY,
                 alias TEXT NOT NULL UNIQUE,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 embedding BLOB,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 session_id TEXT,
                 namespace TEXT,
                 importance REAL,
                 superseded_by TEXT,
                 agent_id TEXT,
                 kind TEXT,
                 pinned INTEGER NOT NULL DEFAULT 0,
                 tenant_id TEXT
             );",
        )
        .unwrap();
        let pref_kind =
            serde_json::to_string(&MemoryKind::Semantic(SemanticSubtype::Preference)).unwrap();
        conn.execute(
            "INSERT INTO memories
                 (id, key, content, category, embedding, created_at, updated_at, session_id, namespace, importance, superseded_by, agent_id, kind, pinned, tenant_id)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, NULL, ?7, NULL, NULL, NULL, ?8, ?9, ?10)",
            rusqlite::params![
                "row-pref-editor",
                "pref_editor",
                "Uses Neovim",
                "core",
                "2026-01-01T00:00:01Z",
                "2026-01-01T00:00:01Z",
                "prefs",
                pref_kind,
                1i64,
                "acme",
            ],
        )
        .unwrap();
    }

    let output = migrate_native(&config_dir, &source, &destination);
    assert!(
        output.status.success(),
        "expected success, got stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!config_dir.exists());

    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("stdout must contain structured JSON receipt");
    assert_eq!(receipt["format"], "zeroclaw.native_brain.migration/v1");
    assert_eq!(receipt["imported_rows"], 1);
    assert!(destination.exists());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let tachi = zeroclaw_memory::tachi::TachiMemory::new("tachi", &dst_ws).unwrap();
        let entry = tachi
            .get("pref_editor")
            .await
            .unwrap()
            .expect("migrated entry");
        assert_eq!(entry.content, "Uses Neovim");
        assert_eq!(entry.namespace, "prefs");
        assert!(entry.pinned);
        assert_eq!(entry.tenant_id.as_deref(), Some("acme"));
    });
}
