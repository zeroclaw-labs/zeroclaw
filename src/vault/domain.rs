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
/// - `Ok(false)` — no domain.db at the path (no-op; the connection
///   runs in single-DB mode against `brain.db` only)
/// - `Err(_)`    — the file existed but ATTACH failed (corrupt DB,
///   permissions, schema-version mismatch, …)
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

// ── meta key/value access ────────────────────────────────────────────
//
// The vault schema's `meta` table is the single source of truth for
// "what version of which domain corpus is installed?" The PR 2 update
// decision tree reads these keys; PR 1 just lays them down so v1
// install paths can start populating them.
//
// Conventional keys (see docs/domain-db-incremental-design.md §2.3):
//   schema_kind        = 'domain'
//   baseline_version   = '<YYYY.MM.DD>'
//   baseline_sha256    = '<64 hex>'
//   current_version    = '<YYYY.MM.DD>' (= baseline_version when no delta applied)
//   last_applied_at    = '<unix seconds>'

/// Snapshot of the install-state keys most callers care about. Helpers
/// return this so a single connection open serves the whole read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainMeta {
    pub schema_kind: Option<String>,
    pub baseline_version: Option<String>,
    pub baseline_sha256: Option<String>,
    pub current_version: Option<String>,
    pub last_applied_at: Option<i64>,
}

impl DomainMeta {
    /// `true` when the installed domain.db has been stamped with the
    /// expected `schema_kind = 'domain'` marker. Pre-v2 domain.db
    /// files (no `meta` table or empty rows) return `false`, which
    /// the PR 2 update path treats as "force a full re-install".
    pub fn is_stamped(&self) -> bool {
        self.schema_kind.as_deref() == Some("domain")
    }
}

/// Read every domain-meta key from the installed `domain.db` (if any).
/// Returns `Default` (`None` everywhere) when the file is missing or
/// when the `meta` table itself is missing — both are indistinguishable
/// from "fresh, unstamped install" for the caller.
pub fn read_meta(workspace_dir: &Path) -> Result<DomainMeta> {
    let path = domain_db_path(workspace_dir);
    if !path.exists() {
        return Ok(DomainMeta::default());
    }
    let conn = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {} read-only", path.display()))?;
    read_meta_from_conn(&conn)
}

/// Lower-level helper used by tests with an in-memory connection. The
/// caller is responsible for ensuring the schema is initialised; on a
/// fresh connection without the `meta` table this returns `Default`.
pub fn read_meta_from_conn(conn: &Connection) -> Result<DomainMeta> {
    // SELECT every row, fold into a struct. Missing keys → None.
    // A missing `meta` table itself (pre-PR-1 domain.db) is treated
    // as "no rows" — the SELECT errors out and we return Default.
    let mut stmt = match conn.prepare("SELECT key, value FROM meta") {
        Ok(s) => s,
        Err(_) => return Ok(DomainMeta::default()),
    };
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .context("querying meta table")?;
    let mut out = DomainMeta::default();
    for kv in rows {
        let (k, v) = kv?;
        match k.as_str() {
            "schema_kind" => out.schema_kind = Some(v),
            "baseline_version" => out.baseline_version = Some(v),
            "baseline_sha256" => out.baseline_sha256 = Some(v),
            "current_version" => out.current_version = Some(v),
            "last_applied_at" => out.last_applied_at = v.parse::<i64>().ok(),
            _ => {} // unknown keys ignored — forward-compat
        }
    }
    Ok(out)
}

/// Stamp the freshly-installed domain.db with its baseline identity.
/// Called by `domain_cli.rs install` (and PR 3's `stamp-baseline`)
/// right after `install_from`. Idempotent — overwrites prior values.
///
/// `schema_kind` is always set to `'domain'` so the client can tell
/// the file apart from a brain.db that happens to share the schema.
pub fn write_baseline_meta(
    path: &Path,
    baseline_version: &str,
    baseline_sha256: &str,
    now_unix: i64,
) -> Result<()> {
    let conn = Connection::open(path)
        .with_context(|| format!("opening {} for meta write", path.display()))?;
    super::schema::init_schema(&conn).context("init schema before writing meta")?;
    write_baseline_meta_on_conn(&conn, baseline_version, baseline_sha256, now_unix)
}

/// Connection-level variant for tests / callers that already hold an
/// open connection (e.g. inside the install transaction).
pub fn write_baseline_meta_on_conn(
    conn: &Connection,
    baseline_version: &str,
    baseline_sha256: &str,
    now_unix: i64,
) -> Result<()> {
    let kvs: &[(&str, String)] = &[
        ("schema_kind", "domain".to_string()),
        ("baseline_version", baseline_version.to_string()),
        ("baseline_sha256", baseline_sha256.to_string()),
        // On a fresh baseline install, current_version == baseline_version.
        // PR 2's apply-delta path bumps current_version on its own.
        ("current_version", baseline_version.to_string()),
        ("last_applied_at", now_unix.to_string()),
    ];
    for (k, v) in kvs {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![k, v],
        )
        .with_context(|| format!("upserting meta[{k}]"))?;
    }
    Ok(())
}

/// Update only the per-delta keys after applying a delta. Used by PR
/// 2's apply-delta path; included here so PR 1 ships the full helper
/// surface and PR 2 is purely consumer code.
pub fn write_delta_meta_on_conn(
    conn: &Connection,
    new_current_version: &str,
    now_unix: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('current_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![new_current_version],
    )?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES ('last_applied_at', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![now_unix.to_string()],
    )?;
    Ok(())
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

    // ── meta helpers ────────────────────────────────────────────────

    #[test]
    fn read_meta_returns_default_when_no_domain_db() {
        let tmp = fresh_workspace();
        let m = read_meta(tmp.path()).unwrap();
        assert_eq!(m, DomainMeta::default());
        assert!(!m.is_stamped());
    }

    #[test]
    fn read_meta_returns_default_on_unstamped_db() {
        // domain.db exists with schema, but no baseline-meta rows yet.
        let tmp = fresh_workspace();
        ensure_schema(&domain_db_path(tmp.path())).unwrap();
        let m = read_meta(tmp.path()).unwrap();
        assert_eq!(m, DomainMeta::default());
        assert!(!m.is_stamped());
    }

    #[test]
    fn read_meta_from_conn_handles_missing_meta_table() {
        // Connection without init_schema → no `meta` table → Default.
        let conn = Connection::open_in_memory().unwrap();
        let m = read_meta_from_conn(&conn).unwrap();
        assert_eq!(m, DomainMeta::default());
    }

    #[test]
    fn write_then_read_baseline_meta_round_trip() {
        let tmp = fresh_workspace();
        let path = domain_db_path(tmp.path());
        ensure_schema(&path).unwrap();
        write_baseline_meta(&path, "2026.01.15", &"a".repeat(64), 1_700_000_000).unwrap();
        let m = read_meta(tmp.path()).unwrap();
        assert!(m.is_stamped());
        assert_eq!(m.baseline_version.as_deref(), Some("2026.01.15"));
        assert_eq!(m.current_version.as_deref(), Some("2026.01.15"));
        assert_eq!(m.baseline_sha256.as_deref(), Some(&*"a".repeat(64)));
        assert_eq!(m.last_applied_at, Some(1_700_000_000));
    }

    #[test]
    fn write_baseline_meta_is_idempotent() {
        let tmp = fresh_workspace();
        let path = domain_db_path(tmp.path());
        ensure_schema(&path).unwrap();
        write_baseline_meta(&path, "2026.01.15", &"a".repeat(64), 1).unwrap();
        // Second call with new values → overwrites cleanly.
        write_baseline_meta(&path, "2027.01.15", &"b".repeat(64), 2).unwrap();
        let m = read_meta(tmp.path()).unwrap();
        assert_eq!(m.baseline_version.as_deref(), Some("2027.01.15"));
        assert_eq!(m.baseline_sha256.as_deref(), Some(&*"b".repeat(64)));
        assert_eq!(m.last_applied_at, Some(2));
    }

    #[test]
    fn write_delta_meta_bumps_only_current_version() {
        let tmp = fresh_workspace();
        let path = domain_db_path(tmp.path());
        ensure_schema(&path).unwrap();
        write_baseline_meta(&path, "2026.01.15", &"a".repeat(64), 1).unwrap();

        // Apply a delta — only current_version + last_applied_at change.
        let conn = Connection::open(&path).unwrap();
        write_delta_meta_on_conn(&conn, "2026.04.22", 100).unwrap();
        drop(conn);

        let m = read_meta(tmp.path()).unwrap();
        assert_eq!(m.baseline_version.as_deref(), Some("2026.01.15")); // untouched
        assert_eq!(m.baseline_sha256.as_deref(), Some(&*"a".repeat(64))); // untouched
        assert_eq!(m.current_version.as_deref(), Some("2026.04.22"));
        assert_eq!(m.last_applied_at, Some(100));
    }
}
