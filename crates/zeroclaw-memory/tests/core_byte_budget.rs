use rusqlite::Connection;
use zeroclaw_config::schema::MemoryConfig;
use zeroclaw_memory::hygiene::run_if_due;
use zeroclaw_memory::traits::StoreOptions;
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};

#[tokio::test]
async fn core_budget_counts_utf8_bytes() {
    let workspace = tempfile::tempdir().unwrap();
    let memory = SqliteMemory::new("default", workspace.path()).unwrap();
    let content = "界".repeat(30);
    for (key, importance) in [("low", 0.1), ("medium", 0.5), ("high", 0.9)] {
        memory
            .store_with_options(
                key,
                &content,
                MemoryCategory::Core,
                None,
                StoreOptions::default().with_importance(importance),
            )
            .await
            .unwrap();
    }
    drop(memory);

    let config = MemoryConfig {
        core_max_bytes: 180,
        ..MemoryConfig::default()
    };
    run_if_due(&config, workspace.path()).unwrap();

    let connection = Connection::open(workspace.path().join("memory/brain.db")).unwrap();
    let mut query = connection
        .prepare("SELECT key, content FROM memories WHERE category = 'core' ORDER BY key")
        .unwrap();
    let rows = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?.len()))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows, [("high".into(), 90), ("medium".into(), 90)]);
}
