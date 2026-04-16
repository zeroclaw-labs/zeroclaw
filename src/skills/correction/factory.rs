//! Factory helpers — wire a CorrectionStore against the shared brain.db.

use super::store::CorrectionStore;
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

fn brain_db_path(workspace_dir: &Path) -> std::path::PathBuf {
    workspace_dir.join("memory").join("brain.db")
}

pub fn build_store(workspace_dir: &Path, device_id: &str) -> Result<Arc<CorrectionStore>> {
    let path = brain_db_path(workspace_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(&path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", 5000i64)?;

    let store = CorrectionStore::new(Arc::new(Mutex::new(conn)), device_id.to_string());
    store.migrate()?;
    Ok(Arc::new(store))
}
