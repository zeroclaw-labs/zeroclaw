//! Factory helpers — wire a UserProfiler against the shared brain.db.

use super::profiler::UserProfiler;
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

fn brain_db_path(workspace_dir: &Path) -> std::path::PathBuf {
    workspace_dir.join("memory").join("brain.db")
}

pub fn build_profiler(workspace_dir: &Path, device_id: &str) -> Result<Arc<UserProfiler>> {
    let path = brain_db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000i64)?;

    let profiler = UserProfiler::new(Arc::new(Mutex::new(conn)), device_id.to_string());
    profiler.migrate()?;
    Ok(Arc::new(profiler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_profiler_creates_and_migrates() {
        let dir = TempDir::new().unwrap();
        let _ = build_profiler(dir.path(), "test-dev").unwrap();
    }
}
