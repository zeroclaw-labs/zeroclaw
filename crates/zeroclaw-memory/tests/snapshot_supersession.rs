use rusqlite::Connection;
use std::fs;
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_memory::snapshot::SNAPSHOT_FILENAME;
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory, create_memory};

#[tokio::test]
async fn startup_snapshot_restores_only_active_core_memories() {
    let source = tempfile::tempdir().unwrap();
    let recovered = tempfile::tempdir().unwrap();
    let memory = SqliteMemory::new("default", source.path()).unwrap();
    for (key, content) in [("old", "Previous status"), ("current", "Current status")] {
        memory
            .store(key, content, MemoryCategory::Core, None)
            .await
            .unwrap();
    }
    let old = memory.get("old").await.unwrap().unwrap();
    let current = memory.get("current").await.unwrap().unwrap();
    memory.supersede(&[old.id], &current.id).await.unwrap();
    drop(memory);

    let config = MemoryConfig {
        backend: "sqlite".into(),
        snapshot_enabled: true,
        snapshot_on_hygiene: true,
        auto_hydrate: true,
        ..MemoryConfig::default()
    };
    drop(create_memory(&config, source.path(), None).unwrap());
    let snapshot_path = source.path().join(SNAPSHOT_FILENAME);
    let exported = fs::read_to_string(&snapshot_path).unwrap();
    fs::copy(snapshot_path, recovered.path().join(SNAPSHOT_FILENAME)).unwrap();
    drop(create_memory(&config, recovered.path(), None).unwrap());

    let connection = Connection::open(recovered.path().join("memory/brain.db")).unwrap();
    let mut query = connection
        .prepare("SELECT key, content FROM memories WHERE superseded_by IS NULL ORDER BY key")
        .unwrap();
    let live = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    assert_eq!(live, [("current".into(), "Current status".into())]);
    assert!(!exported.contains("Previous status"));
    assert!(exported.contains("Current status"));
}
