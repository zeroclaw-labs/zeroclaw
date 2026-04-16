//! Factory helpers — wire a SkillStore against the shared brain.db.

use super::store::SkillStore;
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

/// Default brain.db path inside the workspace.
///
/// This mirrors `memory::sqlite::SqliteMemory`'s DB location so skills,
/// memories, and timeline entries share the same SQLite file when the
/// deployment uses the SQLite memory backend.
pub fn brain_db_path(workspace_dir: &Path) -> std::path::PathBuf {
    workspace_dir.join("memory").join("brain.db")
}

/// Open (or create) brain.db and build a SkillStore rooted in it.
///
/// Runs migration idempotently. Safe to call alongside SqliteMemory —
/// the skill tables coexist with memory_entries/timeline_entries in the
/// same SQLite file.
pub fn build_store(workspace_dir: &Path, device_id: &str) -> Result<Arc<SkillStore>> {
    let path = brain_db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path)?;
    // Set WAL + busy_timeout to play nicely with other readers/writers.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000i64)?;

    let store = SkillStore::new(Arc::new(Mutex::new(conn)), device_id.to_string());
    store.migrate()?;
    Ok(Arc::new(store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_store_creates_db_and_migrates() {
        let dir = TempDir::new().unwrap();
        let store = build_store(dir.path(), "test-dev").unwrap();
        // Should be able to create a skill immediately.
        store
            .create("s1", Some("coding"), "desc", "# content", "agent")
            .unwrap();
        assert!(store.get_by_name("s1").unwrap().is_some());
    }

    #[test]
    fn build_store_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let _ = build_store(dir.path(), "dev-a").unwrap();
        let _ = build_store(dir.path(), "dev-b").unwrap();
    }
}
