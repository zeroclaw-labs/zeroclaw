//! Explicit offline conversion of recognized legacy stores into a new target.
//!
//! The caller must stop every source writer. Original files are opened only
//! through read-only filesystem handles; SQLite sees a private copy, including
//! committed WAL frames. Every failure before publication removes that private
//! work. Publication never replaces a database or occupied sidecar.
//! This bounded migration supports Unix file identity and permissions. The
//! caller must exclusively control the destination directory and prevent other
//! processes from opening the destination until this function returns.

mod format;
mod host;
mod schema;
mod snapshot;
pub(crate) mod source;

#[cfg(all(test, unix))]
mod tests;

use crate::kernel_store::KernelStoreKind;
use anyhow::{Context, Result, bail, ensure};
use memcore::{DbOpenContext, MemoryStore, StoreProfile};
use portable_kernel as memcore;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

pub use source::SourceFileDigest;

const RECEIPT_NAMESPACE: &str = "zeroclaw_kernel_migration";
const TARGET_SUFFIXES: &[&str] = &[
    "",
    "-wal",
    "-shm",
    "-journal",
    ".migration-marker",
    ".identity.json",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDigest {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationVerification {
    pub algorithm: String,
    pub source_tables: Vec<TableDigest>,
    pub target_tables: Vec<TableDigest>,
    pub projection_exclusions: Vec<String>,
    pub source_files: Vec<SourceFileDigest>,
    pub source_schema_sha256: String,
    pub source_snapshot_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReceipt {
    pub format: String,
    pub receipt_namespace: String,
    pub receipt_key: String,
    pub approved_by: String,
    pub created_at: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub source_revision: String,
    pub source_schema: i64,
    pub target_schema: i64,
    pub source_profile: String,
    pub target_profile: String,
    pub role: String,
    pub requires_offline_quiescent_source: bool,
    pub source_row_counts: std::collections::BTreeMap<String, u64>,
    pub recall_cache_rows_invalidated: u64,
    pub verification: MigrationVerification,
}

/// Preserve a recognized source exactly under the documented projection, or
/// refuse without replacing it. This does not change any runtime configuration.
pub fn migrate_kernel_store(
    source: &Path,
    destination: &Path,
    kind: KernelStoreKind,
    approved_by: &str,
) -> Result<MigrationReceipt> {
    ensure!(
        cfg!(unix),
        "offline kernel migration currently supports Unix physical file identity and owner-only permissions only"
    );
    migrate_inner(
        source,
        destination,
        kind,
        approved_by,
        || Ok(()),
        |_| Ok(()),
    )
}

fn migrate_inner(
    source_path: &Path,
    destination: &Path,
    kind: KernelStoreKind,
    approved_by: &str,
    after_source_copy: impl FnOnce() -> Result<()>,
    after_publication: impl FnOnce(&Path) -> Result<()>,
) -> Result<MigrationReceipt> {
    ensure!(
        !approved_by.trim().is_empty(),
        "migration requires a nonblank approved_by value"
    );
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure!(
        parent.is_dir(),
        "migration destination directory must already exist"
    );
    ensure!(
        destination.file_name().is_some(),
        "migration destination must name a file"
    );
    refuse_occupied_target(destination)?;
    let source_metadata =
        std::fs::symlink_metadata(source_path).context("inspect legacy source")?;
    ensure!(
        source_metadata.is_file() && !source_metadata.file_type().is_symlink(),
        "legacy source must be a regular non-symlink file"
    );
    source::physical_identity(&source_metadata)?;
    let original = std::fs::canonicalize(source_path)?;
    let final_parent = std::fs::canonicalize(parent)?;
    let mut directory = HeldDirectory::open(&final_parent)?;
    let final_path = final_parent.join(
        destination
            .file_name()
            .context("destination filename is missing")?,
    );
    ensure!(original != final_path, "source and destination must differ");

    let staging = tempfile::Builder::new()
        .prefix(".zeroclaw-kernel-migration-")
        .tempdir_in(&final_parent)?;
    set_private_permissions(staging.path(), true)?;
    directory.require_private_owner(staging.path())?;
    let copied = staging.path().join("source-copy.db");
    let source_files = source::stable_copy(&original, &copied, after_source_copy)?;
    memcore::db::enable_simple_auto_extension()?;
    memcore::db::register_sqlite_vec();
    let original_snapshot = open_read_only_snapshot(&copied)?;
    let inventory = schema::inspect(&original_snapshot, kind)?;
    snapshot::preflight(&original_snapshot, &inventory, kind)?;
    let expected = snapshot::digests(&original_snapshot, &inventory, None)?;

    // SQLite's backup reads one transaction on the private WAL copy. It never
    // reaches the original path, and uncommitted WAL frames are not imported.
    let snapshot_file = private_tempfile(staging.path(), "snapshot-")?;
    backup(&original_snapshot, snapshot_file.path())?;
    original_snapshot.execute_batch("ROLLBACK")?;
    close_connection(original_snapshot)?;
    checkpoint(snapshot_file.path())?;
    let snapshot_hash = source::file_digest(snapshot_file.path())?;
    let source_snapshot = open_read_only_snapshot(snapshot_file.path())?;
    snapshot::require_equal(
        &expected,
        &snapshot::digests(&source_snapshot, &inventory, None)?,
    )?;

    let target = match kind {
        KernelStoreKind::Memory => {
            let target = private_tempfile(staging.path(), "target-")?;
            let mut store = open_store(target.path(), kind, DbOpenContext::create_fresh())?;
            snapshot::import_memories(&source_snapshot, &inventory, &mut store)?;
            drop(store);
            let mut raw = snapshot::open_auxiliary_target(target.path())?;
            snapshot::copy_auxiliary(&source_snapshot, &inventory, &mut raw)?;
            close_connection(raw)?;
            source_snapshot.execute_batch("ROLLBACK")?;
            close_connection(source_snapshot)?;
            target
        }
        KernelStoreKind::Companion => {
            source_snapshot.execute_batch("ROLLBACK")?;
            close_connection(source_snapshot)?;
            // Only this private, already-censused legacy copy may adopt its
            // first role. The public kernel performs both adoption and upgrade.
            let context = DbOpenContext::open_existing_allow(approved_by)
                .with_exact_profile(StoreProfile::PortableKernel);
            drop(MemoryStore::open_with_label_and_context(
                utf8(snapshot_file.path())?,
                kind.role(),
                &context,
            )?);
            let raw = snapshot::open_auxiliary_target(snapshot_file.path())?;
            raw.execute("DELETE FROM recall_cache", [])?;
            close_connection(raw)?;
            snapshot_file
        }
    };

    let verified_target = validate_target(target.path(), kind, &inventory, &expected, None)?;
    let target_schema = {
        let conn = open_read_only_snapshot(target.path())?;
        let value = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        conn.execute_batch("ROLLBACK")?;
        close_connection(conn)?;
        value
    };
    let receipt_key = uuid::Uuid::new_v4().to_string();
    let receipt = MigrationReceipt {
        format: "zeroclaw.kernel.migration/v1".into(),
        receipt_namespace: RECEIPT_NAMESPACE.into(), receipt_key: receipt_key.clone(),
        approved_by: approved_by.into(), created_at: chrono::Utc::now().to_rfc3339(),
        source: original.clone(), destination: final_path.clone(),
        source_revision: format::LEGACY_REVISION.into(), source_schema: 38, target_schema,
        source_profile: inventory.schema.profile.clone(), target_profile: "portable_kernel".into(),
        role: kind.role().into(), requires_offline_quiescent_source: true,
        source_row_counts: inventory.counts.clone(),
        recall_cache_rows_invalidated: inventory.counts.get("recall_cache").copied().unwrap_or(0),
        verification: MigrationVerification {
            algorithm: "sha256 typed SQLite cells; sorted row digests; length-delimited columns/v1".into(),
            source_tables: expected.clone(), target_tables: verified_target,
            projection_exclusions: vec![
                "memories.rowid and hard_state.rowid are internal; stable identities are id and (namespace,key); all other ordinary rowids and sequence high watermarks are retained".into(),
                "hard_state namespace migrations and store_identity keys profile/role are generated only by the kernel; all other existing hard_state rows are retained".into(),
                "FTS/vec0 physical shadow tables are rebuilt; logical vector bytes are verified; recall_cache rows are explicitly invalidated".into(),
                format!("only the new hard_state receipt {RECEIPT_NAMESPACE}/{receipt_key} is excluded from its own projection"),
            ],
            source_files: source_files.clone(), source_schema_sha256: inventory.schema_sha256.clone(),
            source_snapshot_sha256: snapshot_hash,
        },
    };
    let receipt_json = serde_json::to_string(&receipt)?;
    {
        let store = open_store(target.path(), kind, DbOpenContext::open_existing_deny())?;
        ensure!(
            store.insert_state_if_absent(RECEIPT_NAMESPACE, &receipt_key, &receipt_json)?,
            "new migration receipt key collides with existing history"
        );
    }
    // Receipt insertion is followed by another real exact-role Deny reopen and
    // the same source projection, excluding only the newly inserted key.
    validate_target(
        target.path(),
        kind,
        &inventory,
        &expected,
        Some((&receipt_key, &receipt_json)),
    )?;
    checkpoint(target.path())?;
    set_private_permissions(target.path(), false)?;
    target.as_file().sync_all()?;
    source::verify_unchanged(&original, &source_files)?;
    refuse_occupied_target(&final_path)?;
    publish(
        target,
        &final_path,
        directory,
        Publication {
            kind,
            inventory: &inventory,
            expected: &expected,
            receipt_key: &receipt_key,
            receipt_json: &receipt_json,
            source_path: &original,
            source_files: &source_files,
        },
        after_publication,
    )?;
    Ok(receipt)
}

fn validate_target(
    path: &Path,
    kind: KernelStoreKind,
    inventory: &schema::Inventory,
    expected: &[TableDigest],
    receipt: Option<(&str, &str)>,
) -> Result<Vec<TableDigest>> {
    validate_target_with_boundary(path, kind, inventory, expected, receipt, || Ok(()))
}

fn validate_target_with_boundary(
    path: &Path,
    kind: KernelStoreKind,
    inventory: &schema::Inventory,
    expected: &[TableDigest],
    receipt: Option<(&str, &str)>,
    mut boundary: impl FnMut() -> Result<()>,
) -> Result<Vec<TableDigest>> {
    boundary().context("before target kernel reopen")?;
    {
        let store = open_store(path, kind, DbOpenContext::open_existing_deny())?;
        if let Some((key, json)) = receipt {
            let stored = store
                .get_state_kv(RECEIPT_NAMESPACE, key)?
                .context("migration receipt was not retained")?;
            ensure!(
                stored.0 == json && stored.1 == 1,
                "migration receipt readback differs"
            );
        }
    }
    boundary().context("after target kernel reopen")?;
    // The kernel reopen selects WAL mode. After its writable handle closes,
    // require a clean boundary before switching this owned file back to DELETE.
    // A later read-only WAL connection can leave WAL/SHM files on close even
    // when it made no writes; never classify those as foreign and clean them up.
    checkpoint(path)?;
    boundary().context("after target readback checkpoint")?;
    let conn = open_read_only_snapshot(path)?;
    ensure!(
        schema::stamp(&conn, "profile")?.as_deref() == Some("portable_kernel")
            && schema::stamp(&conn, "role")?.as_deref() == Some(kind.role()),
        "target exact profile/role readback differs"
    );
    schema::verify_target_shape(&conn, inventory)?;
    let actual = snapshot::digests(&conn, inventory, receipt.map(|value| value.0))?;
    snapshot::require_equal(expected, &actual)?;
    let cache_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM recall_cache", [], |row| row.get(0))?;
    ensure!(cache_rows == 0, "target recall cache was not invalidated");
    ensure!(
        conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))? == "ok",
        "target SQLite integrity check failed"
    );
    let mut foreign_keys = conn.prepare("PRAGMA foreign_key_check")?;
    ensure!(
        foreign_keys.query([])?.next()?.is_none(),
        "target foreign-key verification failed"
    );
    drop(foreign_keys);
    conn.execute_batch("ROLLBACK")?;
    close_connection(conn)?;
    boundary().context("after target SQLite readback")?;
    Ok(actual)
}

pub(crate) fn open_store(
    path: &Path,
    kind: KernelStoreKind,
    context: DbOpenContext,
) -> Result<MemoryStore> {
    Ok(MemoryStore::open_with_context(
        utf8(path)?,
        &kind.context(context),
    )?)
}

pub(crate) fn utf8(path: &Path) -> Result<&str> {
    path.to_str().context("kernel database path is not UTF-8")
}

pub(crate) fn open_read_only_snapshot(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED")?;
    // A first read fixes the snapshot before subsequent census/backup queries.
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    Ok(conn)
}

pub(crate) fn close_connection(conn: Connection) -> Result<()> {
    conn.close()
        .map_err(|(_, error)| error)
        .context("close migration SQLite connection")
}

pub(crate) fn backup(source: &Connection, path: &Path) -> Result<()> {
    let mut destination = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    destination.busy_timeout(Duration::from_secs(5))?;
    {
        let backup = rusqlite::backup::Backup::new(source, &mut destination)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match backup.step(128)? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => {}
                rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => bail!("unsupported SQLite backup progress state"),
            }
            ensure!(
                Instant::now() < deadline,
                "offline SQLite snapshot backup exceeded its bounded budget"
            );
        }
    }
    close_connection(destination)
}

pub(crate) fn checkpoint(path: &Path) -> Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let (busy, log, copied): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    ensure!(
        busy == 0 && ((log == -1 && copied == -1) || (log >= 0 && copied == log)),
        "migration checkpoint is incomplete"
    );
    let mode: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    ensure!(
        mode.eq_ignore_ascii_case("delete"),
        "migration target did not leave WAL mode"
    );
    close_connection(conn)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        ensure!(
            !source::suffixed(path, suffix).try_exists()?,
            "migration target retains a SQLite sidecar after checkpoint"
        );
    }
    Ok(())
}

pub(crate) fn private_tempfile(directory: &Path, prefix: &str) -> Result<NamedTempFile> {
    let file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".db")
        .tempfile_in(directory)?;
    set_private_permissions(file.path(), false)?;
    Ok(file)
}

pub(crate) fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_permissions(path, false)?;
    Ok(file)
}

pub(crate) fn set_private_permissions(path: &Path, directory: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, directory);
        bail!("offline kernel migration requires supported owner-only file permissions")
    }
}

pub(crate) fn refuse_occupied_target(path: &Path) -> Result<()> {
    for suffix in TARGET_SUFFIXES {
        match std::fs::symlink_metadata(source::suffixed(path, suffix)) {
            Ok(_) => bail!("migration target database or sidecar is already occupied ({suffix})"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect migration destination"),
        }
    }
    Ok(())
}

pub(crate) struct HeldDirectory {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
    #[cfg(unix)]
    owner_uid: Option<u32>,
}

impl HeldDirectory {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let directory = Self {
            path: path.to_owned(),
            file: File::open(path)?,
            #[cfg(unix)]
            owner_uid: None,
        };
        directory.verify()?;
        Ok(directory)
    }

    pub(crate) fn verify(&self) -> Result<()> {
        let metadata = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && source::physical_identity(&metadata)?
                    == source::physical_identity(&self.file.metadata()?)?,
            "migration destination directory identity changed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = self.file.metadata()?;
            ensure!(
                held.mode() & 0o022 == 0 && self.owner_uid.is_none_or(|owner| held.uid() == owner),
                "migration destination directory ownership or write permissions are unsafe"
            );
        }
        Ok(())
    }

    pub(crate) fn require_private_owner(&mut self, staging: &Path) -> Result<()> {
        self.verify()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let parent = self.file.metadata()?;
            let private = std::fs::symlink_metadata(staging)?;
            ensure!(
                parent.uid() == private.uid() && parent.mode() & 0o022 == 0,
                "migration destination directory must be owned by the caller and not writable by group or others"
            );
            self.owner_uid = Some(private.uid());
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = staging;
            bail!("offline kernel migration currently supports Unix only")
        }
    }
}

struct OwnedPublishedFile {
    path: PathBuf,
    // Keep the originally acquired inode alive even if another process unlinks
    // the path. A newly created inode can never be adopted as our own.
    file: File,
}

impl OwnedPublishedFile {
    fn matches_path(&self) -> Result<bool> {
        let metadata = std::fs::symlink_metadata(&self.path)?;
        Ok(metadata.is_file()
            && !metadata.file_type().is_symlink()
            && source::physical_identity(&metadata)?
                == source::physical_identity(&self.file.metadata()?)?)
    }
}

struct PublishedFiles {
    directory: HeldDirectory,
    files: Vec<OwnedPublishedFile>,
    retained: bool,
}

impl PublishedFiles {
    fn track(&mut self, path: PathBuf, file: File) {
        self.files.push(OwnedPublishedFile { path, file });
    }

    fn verify(&self, destination: &Path) -> Result<()> {
        self.directory.verify()?;
        for owned in &self.files {
            ensure!(
                owned.matches_path()?,
                "published database or marker identity changed"
            );
        }
        // SQLite may create and remove its own temporary sidecars during an
        // open. At each closed-connection boundary there must be none. Existing
        // sidecars are never adopted into this guard or removed by rollback.
        for suffix in ["-wal", "-shm", "-journal", ".identity.json"] {
            match std::fs::symlink_metadata(source::suffixed(destination, suffix)) {
                Ok(_) => bail!(
                    "published target has an occupied sidecar ({suffix}); it has not been adopted or removed"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("inspect published target sidecar"),
            }
        }
        Ok(())
    }
}

impl Drop for PublishedFiles {
    fn drop(&mut self) {
        if self.retained || self.directory.verify().is_err() {
            return;
        }
        for owned in self.files.iter().rev() {
            if owned.matches_path().unwrap_or(false) {
                let _ = std::fs::remove_file(&owned.path);
            }
        }
    }
}

struct Publication<'a> {
    kind: KernelStoreKind,
    inventory: &'a schema::Inventory,
    expected: &'a [TableDigest],
    receipt_key: &'a str,
    receipt_json: &'a str,
    source_path: &'a Path,
    source_files: &'a [SourceFileDigest],
}

pub(crate) fn publish_verified_target(
    target: NamedTempFile,
    destination: &Path,
    parent: HeldDirectory,
    source_path: &Path,
    source_files: &[SourceFileDigest],
    after_publication: impl FnOnce(&Path) -> Result<()>,
    validate_published: impl FnOnce(&Path, &dyn Fn() -> Result<()>) -> Result<()>,
) -> Result<()> {
    let marker_path = source::suffixed(target.path(), ".migration-marker");
    let marker_bytes = std::fs::read(&marker_path)
        .context("kernel did not produce its migration fingerprint marker")?;
    let directory = target
        .path()
        .parent()
        .context("private stage has no parent")?;
    let mut marker = tempfile::Builder::new()
        .prefix("publish-marker-")
        .tempfile_in(directory)?;
    set_private_permissions(marker.path(), false)?;
    marker.write_all(&marker_bytes)?;
    marker.as_file().sync_all()?;
    let mut published = PublishedFiles {
        directory: parent,
        files: Vec::new(),
        retained: false,
    };
    published.directory.verify()?;
    refuse_occupied_target(destination)?;
    let final_marker = source::suffixed(destination, ".migration-marker");
    let marker_file = marker
        .persist_noclobber(&final_marker)
        .map_err(|error| error.error)?;
    published.track(final_marker, marker_file);
    let database_file = target
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    published.track(destination.to_owned(), database_file);
    after_publication(destination)?;
    // A validation failure is rollback of these newly owned files, never a
    // repair of an existing target or an unlink of an unowned replacement.
    validate_published(destination, &|| published.verify(destination))?;
    published.verify(destination)?;
    checkpoint(destination)?;
    published.verify(destination)?;
    source::verify_unchanged(source_path, source_files)?;
    for owned in &published.files {
        owned.file.sync_all()?;
    }
    published.directory.file.sync_all()?;
    published.verify(destination)?;
    published.retained = true;
    Ok(())
}

fn publish(
    target: NamedTempFile,
    destination: &Path,
    parent: HeldDirectory,
    verification: Publication<'_>,
    after_publication: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    publish_verified_target(
        target,
        destination,
        parent,
        verification.source_path,
        verification.source_files,
        after_publication,
        |dest, boundary| {
            validate_target_with_boundary(
                dest,
                verification.kind,
                verification.inventory,
                verification.expected,
                Some((verification.receipt_key, verification.receipt_json)),
                boundary,
            )?;
            Ok(())
        },
    )
}
