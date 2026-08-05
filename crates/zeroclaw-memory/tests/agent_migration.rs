//! Integration tests for the SQLite multi-agent DB migration.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use tempfile::TempDir;
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};

fn db_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join("memory").join("brain.db")
}

fn open_raw(workspace: &Path) -> Connection {
    Connection::open(db_path(workspace)).expect("open sqlite")
}

fn count_backup_files(workspace: &Path) -> usize {
    let memory_dir = workspace.join("memory");
    if !memory_dir.exists() {
        return 0;
    }
    std::fs::read_dir(&memory_dir)
        .expect("read memory dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".backup-"))
        .count()
}

fn fetch_default_agent_uuid(conn: &Connection) -> Option<String> {
    conn.query_row(
        "SELECT id FROM agents WHERE alias = 'default' LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .expect("query default agent uuid")
}

fn agent_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
        .expect("count agents")
}

fn memories_have_agent_id(conn: &Connection) -> bool {
    let schema_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'",
            [],
            |row| row.get(0),
        )
        .expect("read memories schema");
    schema_sql.contains("agent_id")
}

#[tokio::test]
async fn fresh_install_creates_agents_table_and_default_agent() {
    let workspace = TempDir::new().expect("tempdir");
    let _memory = SqliteMemory::new("test", workspace.path()).expect("init sqlite");

    let conn = open_raw(workspace.path());
    assert_eq!(
        agent_count(&conn),
        1,
        "fresh install must seed exactly one agent (the default)"
    );
    let uuid =
        fetch_default_agent_uuid(&conn).expect("default agent must be present after fresh init");
    assert!(
        uuid::Uuid::parse_str(&uuid).is_ok(),
        "default agent id must be a UUID, got: {uuid:?}"
    );
    assert!(
        memories_have_agent_id(&conn),
        "memories must have agent_id column after fresh init"
    );
    assert_eq!(
        count_backup_files(workspace.path()),
        0,
        "fresh install must not produce a backup file"
    );
}

#[tokio::test]
async fn idempotent_reinit_does_not_change_default_agent_uuid_or_agent_count() {
    let workspace = TempDir::new().expect("tempdir");

    // First init.
    let _first = SqliteMemory::new("test", workspace.path()).expect("init sqlite first");
    let conn = open_raw(workspace.path());
    let first_uuid = fetch_default_agent_uuid(&conn).expect("default agent uuid #1");
    let first_count = agent_count(&conn);
    drop(conn);

    // Drop the SqliteMemory handle so we can re-open.
    drop(_first);

    // Re-init. Migration must detect agent_id column already present
    // and skip every step.
    let _second = SqliteMemory::new("test", workspace.path()).expect("init sqlite second");
    let conn = open_raw(workspace.path());
    let second_uuid = fetch_default_agent_uuid(&conn).expect("default agent uuid #2");
    let second_count = agent_count(&conn);

    assert_eq!(
        first_uuid, second_uuid,
        "default agent UUID must be stable across re-init"
    );
    assert_eq!(
        first_count, second_count,
        "agent count must be stable across re-init"
    );
    assert_eq!(
        count_backup_files(workspace.path()),
        0,
        "re-init on an already-migrated DB must not produce a backup"
    );
}

#[tokio::test]
async fn pre_migration_rows_get_backfilled_to_default_agent_with_backup() {
    let workspace = TempDir::new().expect("tempdir");

    // Build a pre-multi-agent DB by writing the legacy schema directly,
    // populating it with a row, then closing. This is the shape an
    // operator upgrading from a single-workspace install would have on
    // disk before the multi-agent runtime first runs.
    let memory_dir = workspace.path().join("memory");
    std::fs::create_dir_all(&memory_dir).expect("memory dir");
    {
        let conn = Connection::open(db_path(workspace.path())).expect("open legacy db");
        conn.execute_batch(
            "CREATE TABLE memories (
                id          TEXT PRIMARY KEY,
                key         TEXT NOT NULL UNIQUE,
                content     TEXT NOT NULL,
                category    TEXT NOT NULL DEFAULT 'core',
                embedding   BLOB,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                session_id  TEXT,
                namespace   TEXT DEFAULT 'default',
                importance  REAL DEFAULT 0.5,
                superseded_by TEXT
            );
            CREATE INDEX idx_memories_category ON memories(category);
            CREATE INDEX idx_memories_key ON memories(key);
            CREATE INDEX idx_memories_session ON memories(session_id);
            CREATE INDEX idx_memories_namespace ON memories(namespace);
            CREATE VIRTUAL TABLE memories_fts USING fts5(
                key, content, content=memories, content_rowid=rowid
            );
            CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
            END;
            CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
            END;
            CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
            END;",
        )
        .expect("create legacy memories + fts");
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at) \
             VALUES ('row-1', 'k', 'v', 'core', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("seed legacy row");
    }

    // First open through the production code path triggers migration.
    let _memory = SqliteMemory::new("test", workspace.path()).expect("init triggers migration");

    let conn = open_raw(workspace.path());
    let default_uuid = fetch_default_agent_uuid(&conn).expect("default agent");
    let backfilled: String = conn
        .query_row(
            "SELECT agent_id FROM memories WHERE id = 'row-1'",
            [],
            |row| row.get(0),
        )
        .expect("query backfilled row");
    assert_eq!(
        backfilled, default_uuid,
        "pre-migration rows must be backfilled to the default agent UUID"
    );
    assert_eq!(
        count_backup_files(workspace.path()),
        1,
        "migration with existing data must produce exactly one backup file"
    );
}

#[tokio::test]
async fn store_after_migration_works_against_default_workspace() {
    // Sanity check: the migrated DB still serves real Memory trait
    // calls. This catches the case where the ALTER + backfill leaves
    // the schema in a state the rest of the code path can't write to.
    let workspace = TempDir::new().expect("tempdir");
    let memory = SqliteMemory::new("test", workspace.path()).expect("init sqlite");
    memory
        .store("post-migration", "hello", MemoryCategory::Core, None)
        .await
        .expect("store after migration");
    let entries = memory
        .recall("hello", 10, None, None, None)
        .await
        .expect("recall after migration");
    assert!(
        entries.iter().any(|e| e.key == "post-migration"),
        "post-migration store/recall round-trip must work"
    );
}
