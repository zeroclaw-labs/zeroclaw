//! Domain DB lifecycle — swappable per-domain corpus split out from `brain.db`.
//!
//! Layout
//! ──────
//! ```text
//! <workspace>/memory/
//! ├── brain.db    ← user's private vault (notes, episodes, wikilinks; always present)
//! └── domain.db   ← swappable domain corpus (legal / medical / bio / …)
//! ```
//!
//! Architecture
//! ────────────
//! - Both DBs share the same schema (`schema::init_schema`).
//! - The domain DB is **ATTACHed as schema `domain`** to the main
//!   connection so cross-DB graph queries can `UNION ALL` across both
//!   without a second connection round-trip.
//! - Writes route by source: user-document ingest goes to `main.*`,
//!   domain bake (`vault legal ingest`, future `vault medical ingest`)
//!   goes to `domain.*`.
//! - Swap = replace the file. The user's `brain.db` is untouched
//!   when a new domain corpus arrives, so personal notes and their
//!   wikilinks stay intact across domain upgrades.
//!
//! This module is purely a lifecycle helper; the schema lives in
//! [`crate::vault::schema`] and the wiring into `VaultStore` lives in
//! `store.rs`.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Conventional filename inside `<workspace>/memory/`.
pub const DOMAIN_FILENAME: &str = "domain.db";
/// SQLite schema name used for the ATTACH binding.
pub const DOMAIN_SCHEMA_NAME: &str = "domain";

/// `<workspace>/memory/domain.db`.
pub fn domain_db_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("memory").join(DOMAIN_FILENAME)
}

/// `true` when a domain corpus is installed at the conventional path.
pub fn is_installed(workspace_dir: &Path) -> bool {
    domain_db_path(workspace_dir).exists()
}

/// Initialise the schema on the domain DB at `path`. Idempotent — safe
/// to call before every ATTACH so a freshly-created file gets the
/// vault tables it needs.
pub fn ensure_schema(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    super::schema::init_schema(&conn)
        .with_context(|| format!("initialising schema on {}", path.display()))?;
    Ok(())
}

/// ATTACH the domain DB at `path` as schema `domain` on `conn`.
///
/// Returns:
/// - `Ok(true)`  — the file existed and was successfully attached
/// - `Ok(false)` — no domain.db at the path (no-op; the connection runs
///                 in single-DB mode against `brain.db` only)
/// - `Err(_)`    — the file existed but ATTACH failed (corrupt DB,
///                 permissions, schema-version mismatch, …)
///
/// **Idempotent**: calling twice with the same `path` is safe — SQLite
/// surfaces the second call as `SQLITE_ERROR: database domain is
/// already in use`, which we translate to `Ok(true)` because the
/// post-condition (domain attached) is satisfied.
pub fn attach(conn: &Connection, path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if is_attached(conn)? {
        return Ok(true);
    }
    // Single-quote-escape the path for the SQL literal.
    let path_str = path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS {};",
        path_str, DOMAIN_SCHEMA_NAME
    ))
    .with_context(|| format!("ATTACH DATABASE {}", path.display()))?;
    Ok(true)
}

/// DETACH the domain DB. No-op when not attached; never errors on
/// "not attached" so callers can use this defensively.
pub fn detach(conn: &Connection) -> Result<()> {
    if !is_attached(conn)? {
        return Ok(());
    }
    conn.execute_batch(&format!("DETACH DATABASE {};", DOMAIN_SCHEMA_NAME))
        .context("DETACH DATABASE domain")?;
    Ok(())
}

/// `true` when the `domain` schema is currently attached on `conn`.
pub fn is_attached(conn: &Connection) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA database_list")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for r in rows {
        if r? == DOMAIN_SCHEMA_NAME {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Replace the workspace's domain.db with the file at `source`. Uses
/// staging + atomic rename so a swap-in-progress crash leaves either
/// the old file or the new file intact, never a half-written one.
///
/// The caller MUST ensure no live `Connection` has `domain.db`
/// ATTACHed before calling — SQLite holds a file handle for the
/// duration of the attach, and Windows in particular will refuse to
/// rename over an open file. Use [`detach`] on every active connection
/// first.
pub fn install_from(workspace_dir: &Path, source: &Path) -> Result<PathBuf> {
    let target = domain_db_path(workspace_dir);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = target.with_extension("db.new");
    std::fs::copy(source, &staging)
        .with_context(|| format!("staging copy {} → {}", source.display(), staging.display()))?;
    std::fs::rename(&staging, &target).with_context(|| {
        format!("atomic rename {} → {}", staging.display(), target.display())
    })?;
    Ok(target)
}

/// Remove the installed domain.db. Idempotent — no error when absent.
/// Caller must ensure the file is not currently ATTACHed.
pub fn uninstall(workspace_dir: &Path) -> Result<()> {
    let path = domain_db_path(workspace_dir);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

/// Read-only inspection of an installed domain.
#[derive(Debug, Clone)]
pub struct DomainInfo {
    pub installed: bool,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub vault_documents_count: i64,
    pub vault_links_count: i64,
}

pub fn info(workspace_dir: &Path) -> Result<DomainInfo> {
    let path = domain_db_path(workspace_dir);
    if !path.exists() {
        return Ok(DomainInfo {
            installed: false,
            path,
            size_bytes: 0,
            vault_documents_count: 0,
            vault_links_count: 0,
        });
    }
    let size_bytes = std::fs::metadata(&path)?.len();
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {} read-only", path.display()))?;
    let vault_documents_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))
        .unwrap_or(0);
    let vault_links_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM vault_links", [], |r| r.get(0))
        .unwrap_or(0);
    Ok(DomainInfo {
        installed: true,
        path,
        size_bytes,
        vault_documents_count,
        vault_links_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_workspace() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn domain_db_path_is_under_memory_subdir() {
        let p = domain_db_path(Path::new("/tmp/ws"));
        assert!(p.ends_with("memory/domain.db"));
    }

    #[test]
    fn is_installed_false_for_empty_workspace() {
        let tmp = fresh_workspace();
        assert!(!is_installed(tmp.path()));
    }

    #[test]
    fn ensure_schema_creates_file_with_vault_tables() {
        let tmp = fresh_workspace();
        let path = domain_db_path(tmp.path());
        ensure_schema(&path).unwrap();
        assert!(path.exists());
        let conn = Connection::open(&path).unwrap();
        // vault_documents table must exist after init.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='vault_documents'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn attach_returns_false_when_no_domain_db() {
        let tmp = fresh_workspace();
        let main_conn = Connection::open_in_memory().unwrap();
        let attached = attach(&main_conn, &domain_db_path(tmp.path())).unwrap();
        assert!(!attached);
        assert!(!is_attached(&main_conn).unwrap());
    }

    #[test]
    fn attach_then_detach_round_trip() {
        let tmp = fresh_workspace();
        ensure_schema(&domain_db_path(tmp.path())).unwrap();

        let main_conn = Connection::open_in_memory().unwrap();
        super::super::schema::init_schema(&main_conn).unwrap();

        let attached = attach(&main_conn, &domain_db_path(tmp.path())).unwrap();
        assert!(attached);
        assert!(is_attached(&main_conn).unwrap());

        // Cross-schema query smoke test — must succeed even with both empty.
        let n: i64 = main_conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM main.vault_documents) + \
                        (SELECT COUNT(*) FROM domain.vault_documents)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);

        detach(&main_conn).unwrap();
        assert!(!is_attached(&main_conn).unwrap());
        // Re-detach is a no-op.
        detach(&main_conn).unwrap();
    }

    #[test]
    fn attach_is_idempotent() {
        let tmp = fresh_workspace();
        ensure_schema(&domain_db_path(tmp.path())).unwrap();
        let main_conn = Connection::open_in_memory().unwrap();
        super::super::schema::init_schema(&main_conn).unwrap();

        attach(&main_conn, &domain_db_path(tmp.path())).unwrap();
        // Second call must NOT error.
        let r = attach(&main_conn, &domain_db_path(tmp.path())).unwrap();
        assert!(r);
        assert!(is_attached(&main_conn).unwrap());
    }

    #[test]
    fn install_from_atomic_rename() {
        let tmp = fresh_workspace();
        let source = tmp.path().join("source.db");
        // Build a valid SQLite file at `source`.
        ensure_schema(&source).unwrap();
        let placed = install_from(tmp.path(), &source).unwrap();
        assert_eq!(placed, domain_db_path(tmp.path()));
        assert!(placed.exists());
        // Staging file is gone.
        assert!(!placed.with_extension("db.new").exists());
    }

    #[test]
    fn uninstall_is_idempotent() {
        let tmp = fresh_workspace();
        // Uninstall when nothing exists — no error.
        uninstall(tmp.path()).unwrap();
        ensure_schema(&domain_db_path(tmp.path())).unwrap();
        assert!(is_installed(tmp.path()));
        uninstall(tmp.path()).unwrap();
        assert!(!is_installed(tmp.path()));
        // Second uninstall — still no error.
        uninstall(tmp.path()).unwrap();
    }

    #[test]
    fn info_reports_empty_when_not_installed() {
        let tmp = fresh_workspace();
        let i = info(tmp.path()).unwrap();
        assert!(!i.installed);
        assert_eq!(i.size_bytes, 0);
        assert_eq!(i.vault_documents_count, 0);
    }

    #[test]
    fn info_reports_counts_for_installed_db() {
        let tmp = fresh_workspace();
        ensure_schema(&domain_db_path(tmp.path())).unwrap();
        // Insert a row so the count is observable.
        let conn = Connection::open(domain_db_path(tmp.path())).unwrap();
        conn.execute(
            "INSERT INTO vault_documents \
                (uuid, title, content, source_type, source_device_id, checksum, \
                 char_count, created_at, updated_at) \
             VALUES ('u1','t','c','local_file','dev','sum',1,1,1)",
            [],
        )
        .unwrap();
        drop(conn);

        let i = info(tmp.path()).unwrap();
        assert!(i.installed);
        assert!(i.size_bytes > 0);
        assert_eq!(i.vault_documents_count, 1);
        assert_eq!(i.vault_links_count, 0);
    }
}
