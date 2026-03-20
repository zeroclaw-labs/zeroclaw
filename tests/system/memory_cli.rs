//! Memory CLI integration tests — verify list/get/stats/clear through the real backend.

use lightwave_sys::config::{Config, MemoryConfig};
use lightwave_sys::memory::{Memory, MemoryCategory};

/// Build a minimal Config pointing at a SQLite memory in a temp dir.
fn test_config(temp_dir: &std::path::Path) -> Config {
    Config {
        memory: MemoryConfig {
            backend: "sqlite".into(),
            auto_save: true,
            ..MemoryConfig::default()
        },
        workspace_dir: temp_dir.to_path_buf(),
        ..Config::default()
    }
}

/// Create a test memory backend from config.
fn test_memory(config: &Config) -> Box<dyn Memory> {
    lightwave_sys::memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )
    .unwrap()
}

/// Memory list returns entries after storing them.
#[tokio::test]
async fn memory_cli_list_returns_stored_entries() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = test_config(temp_dir.path());
    let mem = test_memory(&config);

    mem.store("test_key_1", "first value", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store(
        "test_key_2",
        "second value",
        MemoryCategory::Conversation,
        None,
    )
    .await
    .unwrap();

    let all = mem.list(None, None).await.unwrap();
    assert_eq!(all.len(), 2, "Should have 2 entries");

    // Filter by category
    let core_only = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
    assert_eq!(core_only.len(), 1, "Should have 1 core entry");
    assert_eq!(core_only[0].key, "test_key_1");
}

/// Memory get returns exact match, and None for missing keys.
#[tokio::test]
async fn memory_cli_get_exact_and_missing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = test_config(temp_dir.path());
    let mem = test_memory(&config);

    mem.store("lookup_key", "the content", MemoryCategory::Core, None)
        .await
        .unwrap();

    let found = mem.get("lookup_key").await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().content, "the content");

    let missing = mem.get("nonexistent").await.unwrap();
    assert!(missing.is_none());
}

/// Memory stats: count matches number of stored entries.
#[tokio::test]
async fn memory_cli_stats_count_matches() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = test_config(temp_dir.path());
    let mem = test_memory(&config);

    assert_eq!(mem.count().await.unwrap(), 0);

    mem.store("k1", "v1", MemoryCategory::Core, None)
        .await
        .unwrap();
    mem.store("k2", "v2", MemoryCategory::Daily, None)
        .await
        .unwrap();
    mem.store("k3", "v3", MemoryCategory::Conversation, None)
        .await
        .unwrap();

    assert_eq!(mem.count().await.unwrap(), 3);
    assert!(mem.health_check().await);
}

/// Memory clear (forget) removes entries.
#[tokio::test]
async fn memory_cli_clear_removes_entries() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = test_config(temp_dir.path());
    let mem = test_memory(&config);

    mem.store("to_delete", "ephemeral", MemoryCategory::Conversation, None)
        .await
        .unwrap();
    mem.store("to_keep", "permanent", MemoryCategory::Core, None)
        .await
        .unwrap();

    assert_eq!(mem.count().await.unwrap(), 2);

    let deleted = mem.forget("to_delete").await.unwrap();
    assert!(deleted, "forget should return true for existing key");

    assert_eq!(mem.count().await.unwrap(), 1);
    assert!(mem.get("to_delete").await.unwrap().is_none());
    assert!(mem.get("to_keep").await.unwrap().is_some());

    // Deleting nonexistent key returns false
    let not_found = mem.forget("nonexistent").await.unwrap();
    assert!(!not_found);
}
