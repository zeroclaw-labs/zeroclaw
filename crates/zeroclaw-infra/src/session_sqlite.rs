//! SQLite-backed session persistence with FTS5 search.

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use zeroclaw_api::model_provider::ChatMessage;

/// SQLite-backed session store with FTS5 and WAL mode.
pub struct SqliteSessionBackend {
    conn: Mutex<Connection>,
}

fn committed_jsonl_import_receipts_exist(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM jsonl_import_receipts LIMIT 1)",
        [],
        |row| row.get(0),
    )
    .context("Failed to inspect committed JSONL import receipts")
}

pub(crate) fn has_committed_jsonl_import_receipts(workspace_dir: &Path) -> Result<bool> {
    let db_path = workspace_dir.join("sessions/sessions.db");
    match std::fs::metadata(&db_path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to inspect session DB: {}", db_path.display()));
        }
    }

    let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;
    let receipt_table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master \
             WHERE type = 'table' AND name = 'jsonl_import_receipts')",
            [],
            |row| row.get(0),
        )
        .context("Failed to inspect JSONL import receipt schema")?;
    if !receipt_table_exists {
        return Ok(false);
    }

    committed_jsonl_import_receipts_exist(&conn)
}

impl SqliteSessionBackend {
    /// Open or create the sessions database.
    pub fn new(workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).context("Failed to create sessions directory")?;
        let db_path = sessions_dir.join("sessions.db");

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 4194304;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_key TEXT NOT NULL,
                role        TEXT NOT NULL,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_key ON sessions(session_key);
             CREATE INDEX IF NOT EXISTS idx_sessions_key_id ON sessions(session_key, id);

             CREATE TABLE IF NOT EXISTS session_metadata (
                session_key  TEXT PRIMARY KEY,
                created_at   TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0,
                name         TEXT
             );

             CREATE TABLE IF NOT EXISTS jsonl_import_receipts (
                source_name  TEXT PRIMARY KEY,
                session_key  TEXT NOT NULL,
                source_hash  TEXT NOT NULL,
                source_len   INTEGER NOT NULL,
                imported_at  TEXT NOT NULL
             );

             CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(
                session_key, content, content=sessions, content_rowid=id
             );

             CREATE TRIGGER IF NOT EXISTS sessions_ai AFTER INSERT ON sessions BEGIN
                INSERT INTO sessions_fts(rowid, session_key, content)
                VALUES (new.id, new.session_key, new.content);
             END;
             CREATE TRIGGER IF NOT EXISTS sessions_ad AFTER DELETE ON sessions BEGIN
                INSERT INTO sessions_fts(sessions_fts, rowid, session_key, content)
                VALUES ('delete', old.id, old.session_key, old.content);
             END;
             CREATE TRIGGER IF NOT EXISTS sessions_au AFTER UPDATE ON sessions BEGIN
                INSERT INTO sessions_fts(sessions_fts, rowid, session_key, content)
                VALUES ('delete', old.id, old.session_key, old.content);
                INSERT INTO sessions_fts(rowid, session_key, content)
                VALUES (new.id, new.session_key, new.content);
             END;",
        )
        .context("Failed to initialize session schema")?;

        for (column, ddl) in [
            ("name", "ALTER TABLE session_metadata ADD COLUMN name TEXT"),
            (
                "state",
                "ALTER TABLE session_metadata ADD COLUMN state TEXT NOT NULL DEFAULT 'idle'",
            ),
            (
                "turn_id",
                "ALTER TABLE session_metadata ADD COLUMN turn_id TEXT",
            ),
            (
                "turn_started_at",
                "ALTER TABLE session_metadata ADD COLUMN turn_started_at TEXT",
            ),
            (
                "agent_alias",
                "ALTER TABLE session_metadata ADD COLUMN agent_alias TEXT",
            ),
            (
                "channel_id",
                "ALTER TABLE session_metadata ADD COLUMN channel_id TEXT",
            ),
            (
                "room_id",
                "ALTER TABLE session_metadata ADD COLUMN room_id TEXT",
            ),
            (
                "sender_id",
                "ALTER TABLE session_metadata ADD COLUMN sender_id TEXT",
            ),
        ] {
            Self::ensure_metadata_column(&conn, column, ddl)?;
        }
        for (index, ddl) in [
            (
                "idx_session_metadata_agent_alias",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_agent_alias \
                 ON session_metadata(agent_alias)",
            ),
            (
                "idx_session_metadata_channel_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_channel_id \
                 ON session_metadata(channel_id)",
            ),
            (
                "idx_session_metadata_room_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_room_id \
                 ON session_metadata(room_id)",
            ),
            (
                "idx_session_metadata_sender_id",
                "CREATE INDEX IF NOT EXISTS idx_session_metadata_sender_id \
                 ON session_metadata(sender_id)",
            ),
        ] {
            conn.execute(ddl, [])
                .with_context(|| format!("Failed to create session index {index}"))?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn ensure_metadata_column(conn: &Connection, column: &str, ddl: &str) -> Result<()> {
        let present: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') \
                 WHERE name = ?1",
                params![column],
                |row| row.get(0),
            )
            .with_context(|| format!("Failed to inspect session metadata column {column}"))?;
        if !present {
            conn.execute(ddl, [])
                .with_context(|| format!("Failed to add session metadata column {column}"))?;
        }
        Ok(())
    }

    fn append_on(
        conn: &Connection,
        session_key: &str,
        message: &ChatMessage,
        now: &str,
    ) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT INTO sessions (session_key, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_key, message.role, message.content, now],
        )?;
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(session_key) DO UPDATE SET
                last_activity = excluded.last_activity,
                message_count = message_count + 1",
            params![session_key, now, now],
        )?;
        Ok(())
    }

    fn source_fingerprint(path: &Path, name: &str) -> Result<(String, i64)> {
        let mut file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open JSONL session {name}"))?;
        let mut hasher = Sha256::new();
        let mut source_len = 0_i64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("Failed to read JSONL session {name}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            source_len = source_len
                .checked_add(i64::try_from(read).expect("buffer length fits in i64"))
                .with_context(|| format!("JSONL session {name} is too large to import"))?;
        }
        Ok((format!("{:x}", hasher.finalize()), source_len))
    }

    /// Migrate JSONL session files into SQLite. Renames migrated files to `.jsonl.migrated`.
    pub fn migrate_from_jsonl(&self, workspace_dir: &Path) -> Result<usize> {
        self.migrate_from_jsonl_with_archive(workspace_dir, |from, to, expected| {
            Self::archive_staged_jsonl(from, to, expected)
        })
    }

    fn archive_staged_jsonl(
        staged_path: &Path,
        migrated_path: &Path,
        expected: &(String, i64),
    ) -> Result<()> {
        std::fs::hard_link(staged_path, migrated_path).with_context(|| {
            format!(
                "Failed to create no-clobber JSONL archive {}",
                migrated_path.display()
            )
        })?;
        Self::sync_parent_directory(migrated_path)?;
        let archive_name = migrated_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("archive");
        if &Self::source_fingerprint(migrated_path, archive_name)? != expected {
            let _ = std::fs::remove_file(migrated_path);
            bail!("JSONL archive changed during filesystem handoff");
        }
        std::fs::remove_file(staged_path).with_context(|| {
            format!(
                "Failed to remove staged JSONL source {}",
                staged_path.display()
            )
        })?;
        Self::sync_parent_directory(staged_path)?;
        Ok(())
    }

    fn sync_parent_directory(path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::File::open(parent)
                .with_context(|| {
                    format!(
                        "Failed to open JSONL session directory for sync: {}",
                        parent.display()
                    )
                })?
                .sync_all()
                .with_context(|| {
                    format!(
                        "Failed to sync JSONL session directory: {}",
                        parent.display()
                    )
                })?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    fn sync_staged_jsonl(path: &Path, name: &str) -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        options.write(true);
        options
            .open(path)
            .with_context(|| format!("Failed to open staged JSONL session {name} for sync"))?
            .sync_all()
            .with_context(|| format!("Failed to sync staged JSONL session {name}"))
    }

    fn restore_uncommitted_jsonl(staged_path: &Path, live_path: &Path) -> Result<()> {
        if live_path.exists() {
            bail!(
                "Refusing to replace JSONL session created while {} was staged",
                live_path.display()
            );
        }
        std::fs::rename(staged_path, live_path).with_context(|| {
            format!(
                "Failed to restore empty JSONL session {}",
                live_path.display()
            )
        })
    }

    fn import_receipt(
        conn: &Connection,
        source_name: &str,
    ) -> Result<Option<(String, String, i64)>> {
        conn.query_row(
            "SELECT session_key, source_hash, source_len FROM jsonl_import_receipts \
             WHERE source_name = ?1",
            params![source_name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .with_context(|| format!("Failed to inspect JSONL import receipt for {source_name}"))
    }

    fn reconcile_failed_import<R>(
        &self,
        sessions_dir: &Path,
        mutation_guard: &mut crate::session_store::MutationState,
        name: &str,
        staged_path: &Path,
        live_path: &Path,
        import_error: anyhow::Error,
        receipt_reader: &mut R,
    ) -> anyhow::Error
    where
        R: FnMut(&Connection, &str) -> Result<Option<(String, String, i64)>>,
    {
        let receipt = {
            let conn = self.conn.lock();
            receipt_reader(&conn, name)
        };
        match receipt {
            Ok(Some(_)) => {
                if let Err(state_error) = crate::session_store::mark_session_directory_migrated(
                    sessions_dir,
                    mutation_guard,
                ) {
                    return anyhow::Error::from(state_error).context(format!(
                        "Failed to deactivate JSONL after committed import for {name}: {import_error:#}"
                    ));
                }
                import_error
            }
            Ok(None) => match Self::restore_uncommitted_jsonl(staged_path, live_path) {
                Ok(()) => import_error,
                Err(restore_error) => restore_error.context(format!(
                    "JSONL import for {name} failed before commit: {import_error:#}"
                )),
            },
            Err(receipt_error) => {
                if let Err(state_error) = crate::session_store::mark_session_directory_migrated(
                    sessions_dir,
                    mutation_guard,
                ) {
                    return anyhow::Error::from(state_error).context(format!(
                        "Failed to deactivate JSONL after uncertain import for {name}; receipt inspection also failed: {receipt_error:#}; import error: {import_error:#}"
                    ));
                }
                receipt_error.context(format!(
                    "Could not determine whether failed JSONL import for {name} committed: {import_error:#}"
                ))
            }
        }
    }

    fn migrate_from_jsonl_with_archive<F>(&self, workspace_dir: &Path, archive: F) -> Result<usize>
    where
        F: FnMut(&Path, &Path, &(String, i64)) -> Result<()>,
    {
        self.migrate_from_jsonl_with_handlers(
            workspace_dir,
            Self::source_fingerprint,
            Self::import_receipt,
            archive,
        )
    }

    fn migrate_from_jsonl_with_handlers<S, R, F>(
        &self,
        workspace_dir: &Path,
        mut fingerprint: S,
        mut failed_receipt: R,
        mut archive: F,
    ) -> Result<usize>
    where
        S: FnMut(&Path, &str) -> Result<(String, i64)>,
        R: FnMut(&Connection, &str) -> Result<Option<(String, String, i64)>>,
        F: FnMut(&Path, &Path, &(String, i64)) -> Result<()>,
    {
        let sessions_dir = workspace_dir.join("sessions");
        let mutation_lock = crate::session_store::mutation_lock_for(&sessions_dir)
            .context("Failed to acquire JSONL session mutation lock")?;
        let mut mutation_guard = mutation_lock.lock();
        let receipt_inventory = committed_jsonl_import_receipts_exist(&self.conn.lock());
        let has_committed_import: bool = match receipt_inventory {
            Ok(has_committed_import) => has_committed_import,
            Err(receipt_error) => {
                crate::session_store::mark_session_directory_receipt_state_uncertain(
                    &mut mutation_guard,
                );
                return Err(receipt_error)
                    .context("Failed to inspect committed JSONL import receipts");
            }
        };
        if has_committed_import {
            crate::session_store::mark_session_directory_migrated(
                &sessions_dir,
                &mut mutation_guard,
            )
            .context("Failed to restore JSONL migration state from committed receipts")?;
        } else {
            crate::session_store::clear_session_directory_receipt_state_uncertain(
                &mut mutation_guard,
            );
        }
        let entries = std::fs::read_dir(&sessions_dir)
            .context("Failed to enumerate JSONL sessions for SQLite migration")?;

        let mut candidates: Vec<(String, String, PathBuf, PathBuf, PathBuf, bool)> = Vec::new();
        for entry in entries {
            let entry = entry.context("Failed to inspect JSONL session directory entry")?;
            let file_name = entry.file_name();
            let file_path = Path::new(&file_name);
            let is_live = file_path
                .extension()
                .is_some_and(|extension| extension == "jsonl");
            let is_staged = file_path
                .extension()
                .is_some_and(|extension| extension == "importing")
                && file_path
                    .file_stem()
                    .and_then(|stem| Path::new(stem).extension())
                    .is_some_and(|extension| extension == "jsonl");
            if !is_live && !is_staged {
                continue;
            }
            let name = file_name.into_string().map_err(|_| {
                anyhow::Error::msg(format!(
                    "JSONL migration candidate has a non-UTF-8 filename: {}",
                    entry.path().display()
                ))
            })?;
            let (source_name, staged) = if let Some(source_name) = name.strip_suffix(".importing") {
                if !source_name.ends_with(".jsonl") {
                    continue;
                }
                (source_name.to_owned(), true)
            } else if name.ends_with(".jsonl") {
                (name, false)
            } else {
                continue;
            };
            let key = source_name
                .strip_suffix(".jsonl")
                .expect("candidate suffix checked")
                .to_owned();
            candidates.push((
                source_name.clone(),
                key,
                sessions_dir.join(&source_name),
                sessions_dir.join(format!("{source_name}.importing")),
                sessions_dir.join(format!("{source_name}.migrated")),
                staged,
            ));
        }
        candidates.sort_by(|left, right| right.5.cmp(&left.5).then(left.0.cmp(&right.0)));

        let mut migrated = 0;
        for (name, key, live_path, staged_path, migrated_path, staged) in candidates {
            let candidate_may_have_committed_receipt = staged && has_committed_import;
            if staged {
                if live_path.exists() {
                    bail!(
                        "Refusing ambiguous JSONL migration with both {} and {}",
                        live_path.display(),
                        staged_path.display()
                    );
                }
            } else {
                if staged_path.exists() {
                    bail!(
                        "Refusing ambiguous JSONL migration with both {} and {}",
                        live_path.display(),
                        staged_path.display()
                    );
                }
                if migrated_path.exists() {
                    bail!(
                        "Refusing to replace existing migrated JSONL session {}",
                        migrated_path.display()
                    );
                }
                std::fs::rename(&live_path, &staged_path)
                    .with_context(|| format!("Failed to stage JSONL session {name} for import"))?;
            }

            let prepared_source = (|| -> Result<(String, i64)> {
                Self::sync_staged_jsonl(&staged_path, &name)?;
                Self::sync_parent_directory(&staged_path)?;
                fingerprint(&staged_path, &name)
            })();
            let (source_hash, source_len) = match prepared_source {
                Ok(fingerprint) => fingerprint,
                Err(import_error) => {
                    if candidate_may_have_committed_receipt {
                        return Err(self.reconcile_failed_import(
                            &sessions_dir,
                            &mut mutation_guard,
                            &name,
                            &staged_path,
                            &live_path,
                            import_error,
                            &mut failed_receipt,
                        ));
                    }
                    return match Self::restore_uncommitted_jsonl(&staged_path, &live_path) {
                        Ok(()) => Err(import_error),
                        Err(restore_error) => Err(restore_error.context(format!(
                            "JSONL staging preparation for {name} failed before import: {import_error:#}"
                        ))),
                    };
                }
            };
            let expected = (source_hash.clone(), source_len);
            let mut conn = self.conn.lock();
            if let Err(sync_error) = conn.execute_batch("PRAGMA synchronous = FULL;") {
                drop(conn);
                if candidate_may_have_committed_receipt {
                    return Err(self.reconcile_failed_import(
                        &sessions_dir,
                        &mut mutation_guard,
                        &name,
                        &staged_path,
                        &live_path,
                        sync_error.into(),
                        &mut failed_receipt,
                    ));
                }
                return match Self::restore_uncommitted_jsonl(&staged_path, &live_path) {
                    Ok(()) => Err(sync_error).with_context(|| {
                        format!("Failed to enable durable JSONL import for {name}")
                    }),
                    Err(restore_error) => Err(restore_error.context(format!(
                        "Failed to restore JSONL session {name} after durable import setup failed: {sync_error}"
                    ))),
                };
            }
            let mut receipt_observed_or_commit_attempted = candidate_may_have_committed_receipt;
            let import_result = (|| -> Result<()> {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .with_context(|| {
                        format!("Failed to start JSONL import transaction for {name}")
                    })?;
                let receipt = Self::import_receipt(&tx, &name)?;

                if let Some((receipt_key, receipt_hash, receipt_len)) = receipt {
                    receipt_observed_or_commit_attempted = true;
                    if receipt_key != key
                        || receipt_hash != source_hash
                        || receipt_len != source_len
                    {
                        bail!("JSONL session {name} does not match its committed import receipt");
                    }
                    tx.commit().with_context(|| {
                        format!("Failed to finish JSONL receipt check for {name}")
                    })?;
                    return Ok(());
                }

                let existing_state: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_key = ?1) \
                         OR EXISTS(SELECT 1 FROM session_metadata WHERE session_key = ?1)",
                        params![key],
                        |row| row.get(0),
                    )
                    .with_context(|| {
                        format!("Failed to inspect existing SQLite session state for {key}")
                    })?;
                if existing_state {
                    bail!(
                        "Refusing to import receipt-less JSONL session {name} over existing SQLite session state for {key}"
                    );
                }

                let file = std::fs::File::open(&staged_path)
                    .with_context(|| format!("Failed to open staged JSONL session {name}"))?;
                let now = Utc::now().to_rfc3339();
                let mut inserted = 0_i64;
                let mut has_non_whitespace_source = false;
                {
                    let mut insert = tx
                        .prepare(
                            "INSERT INTO sessions (session_key, role, content, created_at) \
                             VALUES (?1, ?2, ?3, ?4)",
                        )
                        .with_context(|| format!("Failed to prepare JSONL import for {name}"))?;
                    for line in io::BufReader::new(file).split(b'\n') {
                        let line = line.with_context(|| {
                            format!("Failed to read staged JSONL session {name}")
                        })?;
                        let line = match std::str::from_utf8(&line) {
                            Ok(line) => line.trim(),
                            Err(_) => {
                                has_non_whitespace_source = true;
                                continue;
                            }
                        };
                        if line.is_empty() {
                            continue;
                        }
                        has_non_whitespace_source = true;
                        let Ok(message) = serde_json::from_str::<ChatMessage>(line) else {
                            continue;
                        };
                        insert
                            .execute(params![key, message.role, message.content, now])
                            .with_context(|| format!("Failed to import JSONL session {name}"))?;
                        inserted += 1;
                    }
                }

                if inserted == 0 && has_non_whitespace_source {
                    bail!("JSONL session {name} contains no valid messages to import");
                }
                tx.execute(
                    "INSERT INTO session_metadata \
                     (session_key, created_at, last_activity, message_count) \
                     VALUES (?1, ?2, ?3, ?4)",
                    params![key, now, now, inserted],
                )
                .with_context(|| format!("Failed to record metadata for JSONL session {name}"))?;
                tx.execute(
                    "INSERT INTO jsonl_import_receipts \
                     (source_name, session_key, source_hash, source_len, imported_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![name, key, source_hash, source_len, now],
                )
                .with_context(|| format!("Failed to record JSONL import receipt for {name}"))?;
                receipt_observed_or_commit_attempted = true;
                tx.commit()
                    .with_context(|| format!("Failed to commit JSONL import for {name}"))?;
                Ok(())
            })();
            let normal_sync_result = conn
                .execute_batch("PRAGMA synchronous = NORMAL;")
                .context("Failed to restore normal SQLite synchronization mode");

            if let Err(import_error) = import_result {
                drop(conn);
                let import_error = match normal_sync_result {
                    Ok(()) => import_error,
                    Err(sync_error) => sync_error.context(format!(
                        "JSONL import for {name} also failed: {import_error:#}"
                    )),
                };
                if receipt_observed_or_commit_attempted {
                    return Err(self.reconcile_failed_import(
                        &sessions_dir,
                        &mut mutation_guard,
                        &name,
                        &staged_path,
                        &live_path,
                        import_error,
                        &mut failed_receipt,
                    ));
                }
                return match Self::restore_uncommitted_jsonl(&staged_path, &live_path) {
                    Ok(()) => Err(import_error),
                    Err(restore_error) => Err(restore_error.context(format!(
                        "JSONL import for {name} failed before commit: {import_error:#}"
                    ))),
                };
            }
            if let Err(sync_error) = normal_sync_result {
                crate::session_store::mark_session_directory_migrated(
                    &sessions_dir,
                    &mut mutation_guard,
                )
                .context("Failed to deactivate JSONL after committed SQLite import")?;
                drop(conn);
                return Err(sync_error).with_context(|| {
                    format!("JSONL import for {name} committed before sync mode restore failed")
                });
            }
            crate::session_store::mark_session_directory_migrated(
                &sessions_dir,
                &mut mutation_guard,
            )
            .context("Failed to deactivate JSONL after committed SQLite import")?;
            drop(conn);

            if migrated_path.exists() {
                if Self::source_fingerprint(&migrated_path, &name)? != expected {
                    bail!(
                        "Refusing incompatible migrated JSONL session {}",
                        migrated_path.display()
                    );
                }
                Self::sync_parent_directory(&migrated_path)?;
                std::fs::remove_file(&staged_path)
                    .with_context(|| format!("Failed to finish staged JSONL handoff for {name}"))?;
                Self::sync_parent_directory(&staged_path)?;
            } else {
                archive(&staged_path, &migrated_path, &expected)
                    .with_context(|| format!("Failed to archive imported JSONL session {name}"))?;
            }
            if live_path.exists() {
                bail!(
                    "JSONL session {} reappeared during migration",
                    live_path.display()
                );
            }
            migrated += 1;
        }

        Ok(migrated)
    }
}

impl SessionBackend for SqliteSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let conn = self.conn.lock();
        let mut stmt = match conn
            .prepare("SELECT role, content FROM sessions WHERE session_key = ?1 ORDER BY id ASC")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map(params![session_key], |row| {
            Ok(ChatMessage {
                role: row.get(0)?,
                content: row.get(1)?,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn load_with_timestamps(
        &self,
        session_key: &str,
    ) -> Vec<crate::session_backend::TimestampedMessage> {
        use crate::session_backend::TimestampedMessage;
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT role, content, created_at FROM sessions WHERE session_key = ?1 ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map(params![session_key], |row| {
            let role: String = row.get(0)?;
            let content: String = row.get(1)?;
            let created_at_raw: Option<String> = row.get(2).ok();
            let created_at = created_at_raw
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            Ok(TimestampedMessage {
                message: ChatMessage { role, content },
                created_at,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        Self::append_on(&conn, session_key, message, &now).map_err(std::io::Error::other)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let conn = self.conn.lock();

        let last_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM sessions WHERE session_key = ?1 ORDER BY id DESC LIMIT 1",
                params![session_key],
                |row| row.get(0),
            )
            .ok();

        let Some(id) = last_id else {
            return Ok(false);
        };

        conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(std::io::Error::other)?;

        // Update metadata count
        conn.execute(
            "UPDATE session_metadata SET message_count = MAX(0, message_count - 1)
             WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    /// Efficiently update the last message in-place (single UPDATE instead of
    /// DELETE + INSERT). Used for incremental persistence during streaming.
    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        let conn = self.conn.lock();

        let last_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM sessions WHERE session_key = ?1 ORDER BY id DESC LIMIT 1",
                params![session_key],
                |row| row.get(0),
            )
            .ok();

        let Some(id) = last_id else {
            return Ok(false);
        };

        conn.execute(
            "UPDATE sessions SET role = ?1, content = ?2 WHERE id = ?3",
            params![message.role, message.content, id],
        )
        .map_err(std::io::Error::other)?;

        // NOTE: FTS index becomes stale here (no UPDATE trigger, only
        // INSERT/DELETE triggers). This is acceptable — update_last is
        // used for transient streaming snapshots. The final content will
        // be correct in the sessions table for load().

        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE session_metadata SET last_activity = ?1 WHERE session_key = ?2",
            params![now, session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn list_sessions(&self) -> Vec<String> {
        let conn = self.conn.lock();
        let mut stmt = match conn
            .prepare("SELECT session_key FROM session_metadata ORDER BY last_activity DESC")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| row.get(0)) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
             FROM session_metadata ORDER BY last_activity DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let created_str: String = row.get(1)?;
            let activity_str: String = row.get(2)?;
            let count: i64 = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let agent_alias: Option<String> = row.get(5)?;
            let channel_id: Option<String> = row.get(6)?;
            let room_id: Option<String> = row.get(7)?;
            let sender_id: Option<String> = row.get(8)?;

            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let cutoff = (Utc::now() - Duration::hours(i64::from(ttl_hours))).to_rfc3339();

        // Find stale sessions
        let stale_keys: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT session_key FROM session_metadata WHERE last_activity < ?1")
                .map_err(std::io::Error::other)?;
            let rows = stmt
                .query_map(params![cutoff], |row| row.get(0))
                .map_err(std::io::Error::other)?;
            rows.filter_map(|r| r.ok()).collect()
        };

        let count = stale_keys.len();
        for key in &stale_keys {
            let _ = conn.execute("DELETE FROM sessions WHERE session_key = ?1", params![key]);
            let _ = conn.execute(
                "DELETE FROM session_metadata WHERE session_key = ?1",
                params![key],
            );
        }

        Ok(count)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let conn = self.conn.lock();

        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        let count = conn.changes() as usize;

        if count > 0 {
            conn.execute(
                "UPDATE session_metadata SET message_count = 0, last_activity = ?1 WHERE session_key = ?2",
                params![Utc::now().to_rfc3339(), session_key],
            )
            .map_err(std::io::Error::other)?;
        }

        Ok(count)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let conn = self.conn.lock();

        // Check if session exists
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM session_metadata WHERE session_key = ?1",
                params![session_key],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            return Ok(false);
        }

        // Delete messages (FTS5 trigger handles sessions_fts cleanup)
        conn.execute(
            "DELETE FROM sessions WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        // Delete metadata
        conn.execute(
            "DELETE FROM session_metadata WHERE session_key = ?1",
            params![session_key],
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE session_metadata SET agent_alias = NULL WHERE agent_alias = ?1",
                params![agent_alias],
            )
            .map_err(std::io::Error::other)?;
        Ok(rows)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        let conn = self.conn.lock();
        let rows = conn
            .execute(
                "UPDATE session_metadata SET agent_alias = ?2 WHERE agent_alias = ?1",
                params![from, to],
            )
            .map_err(std::io::Error::other)?;
        Ok(rows)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        // Mirror the `WHERE agent_alias = ?1` predicate `rename_agent_attribution`
        // re-points, so the residue probe matches exactly what a resume moves.
        let conn = self.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = ?1",
                params![agent_alias],
                |row| row.get(0),
            )
            .map_err(std::io::Error::other)?;
        Ok(count.max(0) as usize)
    }

    fn session_exists(&self, session_key: &str) -> bool {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT 1 FROM session_metadata WHERE session_key = ?1 LIMIT 1",
            params![session_key],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let name_val = if name.is_empty() { None } else { Some(name) };
        conn.execute(
            "UPDATE session_metadata SET name = ?1 WHERE session_key = ?2",
            params![name_val, session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT name FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| row.get(0),
        )
        .map_err(std::io::Error::other)
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
             FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| {
                let key: String = row.get(0)?;
                let created_str: String = row.get(1)?;
                let activity_str: String = row.get(2)?;
                let count: i64 = row.get(3)?;
                let name: Option<String> = row.get(4)?;
                let agent_alias: Option<String> = row.get(5)?;
                let channel_id: Option<String> = row.get(6)?;
                let room_id: Option<String> = row.get(7)?;
                let sender_id: Option<String> = row.get(8)?;

                let created = DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let activity = DateTime::parse_from_rfc3339(&activity_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                Ok(SessionMetadata {
                    key,
                    name,
                    created_at: created,
                    last_activity: activity,
                    message_count: count as usize,
                    agent_alias,
                    channel_id,
                    room_id,
                    sender_id,
                })
            },
        )
        .ok()
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let now = Utc::now().to_rfc3339();
        let started_at = if state == "running" {
            Some(now.as_str())
        } else {
            None
        };
        conn.execute(
            "UPDATE session_metadata SET state = ?1, turn_id = ?2, turn_started_at = ?3
             WHERE session_key = ?4",
            params![state, turn_id, started_at, session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT state, turn_id, turn_started_at FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| {
                let state: String = row.get(0)?;
                let turn_id: Option<String> = row.get(1)?;
                let started_str: Option<String> = row.get(2)?;
                let turn_started_at = started_str.and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                });
                Ok(SessionState {
                    state,
                    turn_id,
                    turn_started_at,
                })
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(std::io::Error::other(other)),
        })
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
             FROM session_metadata WHERE state = 'running' ORDER BY turn_started_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let created_str: String = row.get(1)?;
            let activity_str: String = row.get(2)?;
            let count: i64 = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let agent_alias: Option<String> = row.get(5)?;
            let channel_id: Option<String> = row.get(6)?;
            let room_id: Option<String> = row.get(7)?;
            let sender_id: Option<String> = row.get(8)?;
            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let conn = self.conn.lock();
        #[allow(clippy::cast_possible_wrap)]
        let cutoff = (Utc::now() - chrono::Duration::seconds(threshold_secs as i64)).to_rfc3339();
        let mut stmt = match conn.prepare(
            "SELECT session_key, created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id
             FROM session_metadata
             WHERE state = 'running' AND turn_started_at < ?1
             ORDER BY turn_started_at ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map(params![cutoff], |row| {
            let key: String = row.get(0)?;
            let created_str: String = row.get(1)?;
            let activity_str: String = row.get(2)?;
            let count: i64 = row.get(3)?;
            let name: Option<String> = row.get(4)?;
            let agent_alias: Option<String> = row.get(5)?;
            let channel_id: Option<String> = row.get(6)?;
            let room_id: Option<String> = row.get(7)?;
            let sender_id: Option<String> = row.get(8)?;
            let created = DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let activity = DateTime::parse_from_rfc3339(&activity_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(SessionMetadata {
                key,
                name,
                created_at: created,
                last_activity: activity,
                message_count: count as usize,
                agent_alias,
                channel_id,
                room_id,
                sender_id,
            })
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.filter_map(|r| r.ok()).collect()
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Some(keyword) = &query.keyword else {
            return self.list_sessions_with_metadata();
        };

        let conn = self.conn.lock();
        #[allow(clippy::cast_possible_wrap)]
        let limit = query.limit.unwrap_or(50) as i64;

        // FTS5 search
        let mut stmt = match conn.prepare(
            "SELECT DISTINCT f.session_key
             FROM sessions_fts f
             WHERE sessions_fts MATCH ?1
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        // Quote each word for FTS5
        let fts_query: String = keyword
            .split_whitespace()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" OR ");

        let keys: Vec<String> = match stmt.query_map(params![fts_query, limit], |row| row.get(0)) {
            Ok(r) => r.filter_map(|r| r.ok()).collect(),
            Err(_) => return Vec::new(),
        };

        // Look up metadata for matched sessions
        keys.iter()
            .filter_map(|key| {
                conn.query_row(
                    "SELECT created_at, last_activity, message_count, name, agent_alias, channel_id, room_id, sender_id FROM session_metadata WHERE session_key = ?1",
                    params![key],
                    |row| {
                        let created_str: String = row.get(0)?;
                        let activity_str: String = row.get(1)?;
                        let count: i64 = row.get(2)?;
                        let name: Option<String> = row.get(3)?;
                        let agent_alias: Option<String> = row.get(4)?;
                        let channel_id: Option<String> = row.get(5)?;
                        let room_id: Option<String> = row.get(6)?;
                        let sender_id: Option<String> = row.get(7)?;
                        Ok(SessionMetadata {
                            key: key.clone(),
                            name,
                            created_at: DateTime::parse_from_rfc3339(&created_str)
                                .map(|dt| dt.with_timezone(&Utc))
                                .unwrap_or_else(|_| Utc::now()),
                            last_activity: DateTime::parse_from_rfc3339(&activity_str)
                                .map(|dt| dt.with_timezone(&Utc))
                                .unwrap_or_else(|_| Utc::now()),
                            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                            message_count: count as usize,
                            agent_alias,
                            channel_id,
                            room_id,
                            sender_id,
                        })
                    },
                )
                .ok()
            })
            .collect()
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let conn = self.conn.lock();
        let alias_val = if agent_alias.is_empty() {
            None
        } else {
            Some(agent_alias)
        };
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count, agent_alias)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(session_key) DO UPDATE SET agent_alias = excluded.agent_alias",
            params![session_key, now, now, alias_val],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT agent_alias FROM session_metadata WHERE session_key = ?1",
            params![session_key],
            |row| row.get(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(std::io::Error::other(other)),
        })
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let conn = self.conn.lock();
        fn normalize(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|s| !s.is_empty())
        }
        let channel_id = normalize(context.channel_id);
        let room_id = normalize(context.room_id);
        let sender_id = normalize(context.sender_id);
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO session_metadata
                (session_key, created_at, last_activity, message_count, channel_id, room_id, sender_id)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)
             ON CONFLICT(session_key) DO UPDATE SET
                channel_id = COALESCE(excluded.channel_id, session_metadata.channel_id),
                room_id    = COALESCE(excluded.room_id,    session_metadata.room_id),
                sender_id  = COALESCE(excluded.sender_id,  session_metadata.sender_id)",
            params![session_key, now, now, channel_id, room_id, sender_id],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_store::SessionStore;
    use std::sync::{Arc, mpsc};
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;

    #[test]
    fn round_trip_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append("user1", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .append("user1", &ChatMessage::assistant("hi"))
            .unwrap();

        let msgs = backend.load("user1");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }

    #[test]
    fn remove_last_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("u", &ChatMessage::user("a")).unwrap();
        backend.append("u", &ChatMessage::user("b")).unwrap();

        assert!(backend.remove_last("u").unwrap());
        let msgs = backend.load("u");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "a");
    }

    #[test]
    fn remove_last_empty_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(!backend.remove_last("nonexistent").unwrap());
    }

    #[test]
    fn list_sessions_sqlite() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("a", &ChatMessage::user("hi")).unwrap();
        backend.append("b", &ChatMessage::user("hey")).unwrap();

        let sessions = backend.list_sessions();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn metadata_tracks_counts() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("a")).unwrap();
        backend.append("s1", &ChatMessage::user("b")).unwrap();
        backend.append("s1", &ChatMessage::user("c")).unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_count, 3);
    }

    #[test]
    fn fts5_search_finds_content() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append(
                "code_chat",
                &ChatMessage::user("How do I parse JSON in Rust?"),
            )
            .unwrap();
        backend
            .append("weather", &ChatMessage::user("What's the weather today?"))
            .unwrap();

        let results = backend.search(&SessionQuery {
            keyword: Some("Rust".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "code_chat");
    }

    #[test]
    fn fts5_update_trigger_syncs_index() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .append("chat", &ChatMessage::user("hello world"))
            .unwrap();

        // Verify initial content is searchable
        let results = backend.search(&SessionQuery {
            keyword: Some("hello".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "chat");

        // Directly update the session content (simulates update_last behavior)
        {
            let conn = backend.conn.lock();
            conn.execute(
                "UPDATE sessions SET content = ?1 WHERE session_key = ?2",
                params!["goodbye world", "chat"],
            )
            .unwrap();
        }

        // Old keyword should no longer match
        let results = backend.search(&SessionQuery {
            keyword: Some("hello".into()),
            limit: Some(10),
        });
        assert!(results.is_empty());

        // New keyword should match after UPDATE trigger syncs FTS index
        let results = backend.search(&SessionQuery {
            keyword: Some("goodbye".into()),
            limit: Some(10),
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "chat");
    }

    #[test]
    fn cleanup_stale_removes_old_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // Insert a session with old timestamp
        {
            let conn = backend.conn.lock();
            let old_time = (Utc::now() - Duration::hours(100)).to_rfc3339();
            conn.execute(
                "INSERT INTO sessions (session_key, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
                params!["old_session", "user", "ancient", old_time],
            ).unwrap();
            conn.execute(
                "INSERT INTO session_metadata (session_key, created_at, last_activity, message_count) VALUES (?1, ?2, ?3, 1)",
                params!["old_session", old_time, old_time],
            ).unwrap();
        }

        backend
            .append("new_session", &ChatMessage::user("fresh"))
            .unwrap();

        let cleaned = backend.cleanup_stale(48).unwrap(); // 48h TTL
        assert_eq!(cleaned, 1);

        let sessions = backend.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0], "new_session");
    }

    #[test]
    fn clear_messages_removes_rows_keeps_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.set_session_name("s1", "My Session").unwrap();

        let cleared = backend.clear_messages("s1").unwrap();
        assert_eq!(cleared, 2);
        assert!(backend.load("s1").is_empty());
        // Session still exists in metadata with name preserved
        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_count, 0);
        assert_eq!(meta[0].name.as_deref(), Some("My Session"));
    }

    #[test]
    fn clear_messages_empty_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.clear_messages("nonexistent").unwrap(), 0);
    }

    #[test]
    fn clear_messages_does_not_affect_other_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s2", &ChatMessage::user("world")).unwrap();

        backend.clear_messages("s1").unwrap();
        assert!(backend.load("s1").is_empty());
        assert_eq!(backend.load("s2").len(), 1);
    }

    #[test]
    fn clear_messages_then_append_works() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("old")).unwrap();
        backend.clear_messages("s1").unwrap();
        backend.append("s1", &ChatMessage::user("new")).unwrap();

        let messages = backend.load("s1");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "new");
        // Metadata count should reflect the new message
        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta[0].message_count, 1);
    }

    #[test]
    fn delete_session_removes_all_data() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.append("s2", &ChatMessage::user("other")).unwrap();

        assert!(backend.delete_session("s1").unwrap());
        assert!(backend.load("s1").is_empty());
        assert_eq!(backend.list_sessions().len(), 1);
        assert_eq!(backend.list_sessions()[0], "s2");
    }

    #[test]
    fn delete_session_returns_false_for_missing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(!backend.delete_session("nonexistent").unwrap());
    }

    #[test]
    fn session_exists_tracks_metadata_row() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        assert!(!backend.session_exists("ghost"));

        backend
            .append("ghost", &ChatMessage::user("first"))
            .unwrap();
        assert!(backend.session_exists("ghost"));

        assert!(backend.delete_session("ghost").unwrap());
        assert!(!backend.session_exists("ghost"));
    }

    #[test]
    fn migrate_from_jsonl_imports_and_renames() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Create a JSONL file
        let jsonl_path = sessions_dir.join("test_user.jsonl");
        std::fs::write(
            &jsonl_path,
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"hi\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let migrated = backend.migrate_from_jsonl(tmp.path()).unwrap();
        assert_eq!(migrated, 1);

        // JSONL should be renamed
        assert!(!jsonl_path.exists());
        assert!(sessions_dir.join("test_user.jsonl.migrated").exists());

        // Messages should be in SQLite
        let msgs = backend.load("test_user");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn migrate_from_jsonl_preserves_cleared_and_whitespace_only_sessions() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        let store = SessionStore::new(tmp.path()).unwrap();
        store
            .append("cleared_user", &ChatMessage::user("before clear"))
            .unwrap();
        assert_eq!(store.clear_messages("cleared_user").unwrap(), 1);
        std::fs::write(
            sessions_dir.join("whitespace_user.jsonl"),
            " \n\t\n\u{2003}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 2);

        assert!(backend.load("cleared_user").is_empty());
        assert!(backend.load("whitespace_user").is_empty());
        let mut sessions = backend.list_sessions();
        sessions.sort();
        assert_eq!(sessions, vec!["cleared_user", "whitespace_user"]);
        for key in ["cleared_user", "whitespace_user"] {
            assert!(!sessions_dir.join(format!("{key}.jsonl")).exists());
            assert!(sessions_dir.join(format!("{key}.jsonl.migrated")).exists());
        }

        let conn = backend.conn.lock();
        let zero_count_metadata: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_metadata WHERE message_count = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let receipt_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jsonl_import_receipts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(zero_count_metadata, 2);
        assert_eq!(receipt_count, 2);
    }

    #[test]
    fn migrate_from_jsonl_rejects_invalid_utf8_without_messages() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let jsonl_path = sessions_dir.join("invalid_utf8.jsonl");
        std::fs::write(&jsonl_path, [0xff, b'\n']).unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();

        assert!(err.to_string().contains("no valid messages"));
        assert!(jsonl_path.exists());
        assert!(!sessions_dir.join("invalid_utf8.jsonl.importing").exists());
        assert!(!sessions_dir.join("invalid_utf8.jsonl.migrated").exists());
        assert!(backend.list_sessions().is_empty());
    }

    #[test]
    fn migrate_from_jsonl_retries_handoff_without_duplicate_rows() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let jsonl_path = sessions_dir.join("retry_user.jsonl");
        std::fs::write(
            &jsonl_path,
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"hi\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let err = backend
            .migrate_from_jsonl_with_archive(tmp.path(), |_, _, _| {
                bail!("injected archive failure")
            })
            .unwrap_err();

        assert!(format!("{err:#}").contains("injected archive failure"));
        assert!(!jsonl_path.exists());
        assert!(sessions_dir.join("retry_user.jsonl.importing").exists());
        assert_eq!(backend.load("retry_user").len(), 2);
        drop(backend);

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 1);
        assert!(!jsonl_path.exists());
        assert!(sessions_dir.join("retry_user.jsonl.migrated").exists());
        assert_eq!(backend.load("retry_user").len(), 2);
    }

    #[test]
    fn migrate_from_jsonl_rejects_changed_source_after_committed_import() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let jsonl_path = sessions_dir.join("changed_user.jsonl");
        std::fs::write(&jsonl_path, "{\"role\":\"user\",\"content\":\"hello\"}\n").unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .migrate_from_jsonl_with_archive(tmp.path(), |_, _, _| {
                bail!("injected archive failure")
            })
            .unwrap_err();
        std::fs::write(
            sessions_dir.join("changed_user.jsonl.importing"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"new\"}\n",
        )
        .unwrap();

        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("committed import receipt"));
        assert!(!jsonl_path.exists());
        assert_eq!(backend.load("changed_user").len(), 1);
    }

    #[test]
    fn migrate_from_jsonl_recovers_after_archive_link_crash() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("linked_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .migrate_from_jsonl_with_archive(tmp.path(), |from, to, _| {
                std::fs::hard_link(from, to)?;
                bail!("injected crash after archive link")
            })
            .unwrap_err();
        assert!(sessions_dir.join("linked_user.jsonl.importing").exists());
        assert!(sessions_dir.join("linked_user.jsonl.migrated").exists());
        drop(backend);

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let retry_error = backend
            .migrate_from_jsonl_with_handlers(
                tmp.path(),
                |_, _| bail!("injected retry fingerprint failure"),
                SqliteSessionBackend::import_receipt,
                SqliteSessionBackend::archive_staged_jsonl,
            )
            .unwrap_err();
        assert!(format!("{retry_error:#}").contains("injected retry fingerprint failure"));
        assert!(!sessions_dir.join("linked_user.jsonl").exists());
        assert!(sessions_dir.join("linked_user.jsonl.importing").exists());
        assert!(sessions_dir.join("linked_user.jsonl.migrated").exists());

        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 1);
        assert!(!sessions_dir.join("linked_user.jsonl.importing").exists());
        assert_eq!(backend.load("linked_user").len(), 1);
    }

    #[test]
    fn migrate_from_jsonl_rejects_receipt_for_different_session_key() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("receipt_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .migrate_from_jsonl_with_archive(tmp.path(), |_, _, _| {
                bail!("injected archive failure")
            })
            .unwrap_err();
        backend
            .conn
            .lock()
            .execute(
                "UPDATE jsonl_import_receipts SET session_key = 'other' \
                 WHERE source_name = 'receipt_user.jsonl'",
                [],
            )
            .unwrap();

        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("committed import receipt"));
        assert!(sessions_dir.join("receipt_user.jsonl.importing").exists());
    }

    #[test]
    fn migrate_from_jsonl_rejects_receiptless_non_empty_session() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("legacy_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .append("legacy_user", &ChatMessage::user("already imported"))
            .unwrap();
        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();

        assert!(err.to_string().contains("receipt-less JSONL session"));
        assert!(sessions_dir.join("legacy_user.jsonl").exists());
        assert!(!sessions_dir.join("legacy_user.jsonl.importing").exists());
        assert_eq!(backend.load("legacy_user").len(), 1);
    }

    #[test]
    fn migrate_from_jsonl_rejects_receiptless_metadata_only_session() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("legacy_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"legacy\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .append("legacy_user", &ChatMessage::user("cleared"))
            .unwrap();
        backend
            .set_session_name("legacy_user", "Preserved name")
            .unwrap();
        assert_eq!(backend.clear_messages("legacy_user").unwrap(), 1);

        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();

        assert!(err.to_string().contains("existing SQLite session state"));
        assert!(sessions_dir.join("legacy_user.jsonl").exists());
        assert!(!sessions_dir.join("legacy_user.jsonl.importing").exists());
        assert!(backend.load("legacy_user").is_empty());
        let metadata = backend.get_session_metadata("legacy_user").unwrap();
        assert_eq!(metadata.message_count, 0);
        assert_eq!(metadata.name.as_deref(), Some("Preserved name"));
    }

    #[test]
    fn migrate_from_jsonl_keeps_jsonl_inactive_after_partial_batch_failure() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("a_import.jsonl"),
            "{\"role\":\"user\",\"content\":\"imported\"}\n",
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("b_collision.jsonl"),
            "{\"role\":\"user\",\"content\":\"legacy\"}\n",
        )
        .unwrap();

        let store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let backend = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        backend
            .append("b_collision", &ChatMessage::user("existing"))
            .unwrap();

        let workspace = tmp.path().to_path_buf();
        let (archive_started_tx, archive_started_rx) = mpsc::channel();
        let (release_archive_tx, release_archive_rx) = mpsc::channel();
        let migration_backend = Arc::clone(&backend);
        let migration = std::thread::spawn(move || {
            migration_backend.migrate_from_jsonl_with_archive(&workspace, |from, to, expected| {
                archive_started_tx.send(()).unwrap();
                release_archive_rx.recv().unwrap();
                SqliteSessionBackend::archive_staged_jsonl(from, to, expected)
            })
        });

        archive_started_rx.recv().unwrap();
        let writer_store = Arc::clone(&store);
        let writer = std::thread::spawn(move || {
            writer_store.append("a_import", &ChatMessage::user("late JSONL write"))
        });
        release_archive_tx.send(()).unwrap();

        let migration_error = migration.join().unwrap().unwrap_err();
        assert!(
            migration_error
                .to_string()
                .contains("existing SQLite session state")
        );
        let writer_error = writer.join().unwrap().unwrap_err();
        assert!(
            writer_error
                .to_string()
                .contains("inactive after SQLite migration")
        );
        assert_eq!(backend.load("a_import").len(), 1);
        assert!(!sessions_dir.join("a_import.jsonl").exists());
        assert!(sessions_dir.join("a_import.jsonl.migrated").exists());

        drop(store);
        drop(backend);
        crate::session_store::forget_session_directory_migration_state_for_test(&sessions_dir)
            .unwrap();

        let reopened_store = SessionStore::new(tmp.path()).unwrap();
        let reopened_backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let retry_error = reopened_backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(
            retry_error
                .to_string()
                .contains("existing SQLite session state")
        );
        let reopened_error = reopened_store
            .append("a_import", &ChatMessage::user("later"))
            .unwrap_err();
        assert!(
            reopened_error
                .to_string()
                .contains("inactive after SQLite migration")
        );
        assert!(sessions_dir.join("b_collision.jsonl").exists());
        assert!(!sessions_dir.join("b_collision.jsonl.importing").exists());
    }

    #[test]
    fn migrate_from_jsonl_skips_non_utf8_lines() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let mut source = b"{\"role\":\"user\",\"content\":\"hello\"}\n".to_vec();
        source.extend_from_slice(&[0xff, 0xfe, b'\n']);
        source.extend_from_slice(b"{\"role\":\"assistant\",\"content\":\"hi\"}\n");
        std::fs::write(sessions_dir.join("mixed_user.jsonl"), source).unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 1);
        assert_eq!(backend.load("mixed_user").len(), 2);
    }

    #[test]
    fn migrate_from_jsonl_restores_source_with_no_valid_messages() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let live_path = sessions_dir.join("empty_user.jsonl");
        std::fs::write(&live_path, b"not json\n\xff\n").unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("no valid messages"));
        assert!(live_path.exists());
        assert!(!sessions_dir.join("empty_user.jsonl.importing").exists());
    }

    #[test]
    fn migrate_from_jsonl_does_not_clobber_archive_created_during_handoff() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let migrated_path = sessions_dir.join("raced_user.jsonl.migrated");
        std::fs::write(
            sessions_dir.join("raced_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let err = backend
            .migrate_from_jsonl_with_archive(tmp.path(), |from, to, expected| {
                std::fs::write(to, "concurrent archive")?;
                SqliteSessionBackend::archive_staged_jsonl(from, to, expected)
            })
            .unwrap_err();

        assert!(format!("{err:#}").contains("no-clobber JSONL archive"));
        assert_eq!(
            std::fs::read_to_string(migrated_path).unwrap(),
            "concurrent archive"
        );
        assert!(sessions_dir.join("raced_user.jsonl.importing").exists());
    }

    #[test]
    fn migrate_from_jsonl_blocks_jsonl_append_until_handoff_finishes() {
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        store
            .append("locked_user", &ChatMessage::user("before"))
            .unwrap();
        let backend = Arc::new(SqliteSessionBackend::new(tmp.path()).unwrap());
        let workspace = tmp.path().to_path_buf();
        let (archive_started_tx, archive_started_rx) = mpsc::channel();
        let (release_archive_tx, release_archive_rx) = mpsc::channel();
        let migration_backend = Arc::clone(&backend);
        let migration = std::thread::spawn(move || {
            migration_backend.migrate_from_jsonl_with_archive(&workspace, |from, to, expected| {
                archive_started_tx.send(()).unwrap();
                release_archive_rx.recv().unwrap();
                SqliteSessionBackend::archive_staged_jsonl(from, to, expected)
            })
        });

        archive_started_rx.recv().unwrap();
        let (append_done_tx, append_done_rx) = mpsc::channel();
        let append_store = Arc::clone(&store);
        let append = std::thread::spawn(move || {
            let result = append_store.append("locked_user", &ChatMessage::user("after"));
            append_done_tx.send(()).unwrap();
            result
        });
        assert!(
            append_done_rx
                .recv_timeout(StdDuration::from_millis(100))
                .is_err()
        );

        release_archive_tx.send(()).unwrap();
        assert_eq!(migration.join().unwrap().unwrap(), 1);
        let append_error = append.join().unwrap().unwrap_err();
        assert!(
            append_error
                .to_string()
                .contains("inactive after SQLite migration")
        );
        assert_eq!(backend.load("locked_user").len(), 1);
        assert!(store.load("locked_user").is_empty());
        assert!(!tmp.path().join("sessions/locked_user.jsonl").exists());

        let reopened_store = SessionStore::new(tmp.path()).unwrap();
        let error = reopened_store
            .append("locked_user", &ChatMessage::user("later"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("inactive after SQLite migration")
        );
    }

    #[test]
    fn session_store_restores_migration_fence_from_receipt_after_restart() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        let store = SessionStore::new(tmp.path()).unwrap();
        store
            .append("restart_user", &ChatMessage::user("before migration"))
            .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 1);
        drop(store);
        drop(backend);
        crate::session_store::forget_session_directory_migration_state_for_test(&sessions_dir)
            .unwrap();

        let reopened_store = SessionStore::new(tmp.path()).unwrap();
        let error = reopened_store
            .append("restart_user", &ChatMessage::user("after restart"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("inactive after SQLite migration")
        );
        assert!(!sessions_dir.join("restart_user.jsonl").exists());

        let reopened_backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        reopened_backend
            .conn
            .lock()
            .execute("DELETE FROM jsonl_import_receipts", [])
            .unwrap();
        assert_eq!(reopened_backend.migrate_from_jsonl(tmp.path()).unwrap(), 0);
        let error = reopened_store
            .append("restart_user", &ChatMessage::user("after receipt removal"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("inactive after SQLite migration")
        );
    }

    #[test]
    fn session_store_fails_closed_when_migration_state_is_unreadable() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(sessions_dir.join("sessions.db"), "not a sqlite database").unwrap();

        let error = match SessionStore::new(tmp.path()) {
            Ok(_) => panic!("corrupt receipt state must reject JSONL construction"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("Failed to inspect durable JSONL migration state")
        );

        std::fs::remove_file(sessions_dir.join("sessions.db")).unwrap();
        let recovered_store = SessionStore::new(tmp.path()).unwrap();
        recovered_store
            .append("recovered", &ChatMessage::user("receipt-free"))
            .unwrap();
    }

    #[test]
    fn migrate_from_jsonl_rejects_existing_archive_before_import() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("collision_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("collision_user.jsonl.migrated"),
            "prior archive",
        )
        .unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let err = backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("Refusing to replace"));
        assert!(backend.load("collision_user").is_empty());
    }

    #[test]
    fn migrate_from_jsonl_rolls_back_messages_metadata_and_receipt_together() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("rollback_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"hello\"}\n{\"role\":\"assistant\",\"content\":\"fail\"}\n",
        )
        .unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        {
            let conn = backend.conn.lock();
            conn.execute_batch(
                "CREATE TRIGGER reject_assistant_import BEFORE INSERT ON sessions
                 WHEN NEW.role = 'assistant' BEGIN
                    SELECT RAISE(ABORT, 'injected import failure');
                 END;",
            )
            .unwrap();
        }

        let error = backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("injected import failure"));
        assert!(backend.load("rollback_user").is_empty());
        let (metadata_count, receipt_count) = {
            let conn = backend.conn.lock();
            let metadata_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM session_metadata WHERE session_key = 'rollback_user'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let receipt_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM jsonl_import_receipts WHERE source_name = 'rollback_user.jsonl'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (metadata_count, receipt_count)
        };
        assert_eq!(metadata_count, 0);
        assert_eq!(receipt_count, 0);
        assert!(sessions_dir.join("rollback_user.jsonl").exists());
        assert!(!sessions_dir.join("rollback_user.jsonl.importing").exists());

        store
            .append("rollback_user", &ChatMessage::user("after failure"))
            .unwrap();
        assert_eq!(store.load("rollback_user").len(), 3);
    }

    #[test]
    fn migrate_from_jsonl_restores_source_after_fingerprint_failure() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("fingerprint_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"before\"}\n",
        )
        .unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let error = backend
            .migrate_from_jsonl_with_handlers(
                tmp.path(),
                |_, _| bail!("injected fingerprint failure"),
                SqliteSessionBackend::import_receipt,
                SqliteSessionBackend::archive_staged_jsonl,
            )
            .unwrap_err();

        assert!(format!("{error:#}").contains("injected fingerprint failure"));
        assert!(sessions_dir.join("fingerprint_user.jsonl").exists());
        assert!(
            !sessions_dir
                .join("fingerprint_user.jsonl.importing")
                .exists()
        );
        store
            .append("fingerprint_user", &ChatMessage::user("after failure"))
            .unwrap();
        assert_eq!(store.load("fingerprint_user").len(), 2);
    }

    #[test]
    fn migrate_from_jsonl_does_not_query_receipt_after_fingerprint_failure() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("uncertain_user.jsonl"),
            "{\"role\":\"user\",\"content\":\"before\"}\n",
        )
        .unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let error = backend
            .migrate_from_jsonl_with_handlers(
                tmp.path(),
                |_, _| bail!("injected fingerprint failure"),
                |_, _| panic!("receipt query must not run before an import transaction"),
                SqliteSessionBackend::archive_staged_jsonl,
            )
            .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("injected fingerprint failure"));
        assert!(sessions_dir.join("uncertain_user.jsonl").exists());
        assert!(!sessions_dir.join("uncertain_user.jsonl.importing").exists());
        store
            .append("uncertain_user", &ChatMessage::user("after failure"))
            .unwrap();
    }

    #[test]
    fn migrate_from_jsonl_temporarily_blocks_writer_until_receipt_inventory_recovers() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend
            .conn
            .lock()
            .execute("DROP TABLE jsonl_import_receipts", [])
            .unwrap();

        let error = backend.migrate_from_jsonl(tmp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("Failed to inspect committed JSONL import receipts"));
        let append_error = store
            .append("uncertain_user", &ChatMessage::user("after failure"))
            .unwrap_err();
        assert!(
            append_error
                .to_string()
                .contains("inactive after SQLite migration")
        );

        backend
            .conn
            .lock()
            .execute_batch(
                "CREATE TABLE jsonl_import_receipts (
                    source_name  TEXT PRIMARY KEY,
                    session_key  TEXT NOT NULL,
                    source_hash  TEXT NOT NULL,
                    source_len   INTEGER NOT NULL,
                    imported_at  TEXT NOT NULL
                );",
            )
            .unwrap();
        assert_eq!(backend.migrate_from_jsonl(tmp.path()).unwrap(), 0);
        store
            .append("uncertain_user", &ChatMessage::user("after repair"))
            .unwrap();
    }

    #[test]
    fn new_rejects_schema_index_name_collisions() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let conn = Connection::open(sessions_dir.join("sessions.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE idx_session_metadata_agent_alias (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        drop(conn);

        let err = SqliteSessionBackend::new(tmp.path())
            .err()
            .expect("index name collision must fail startup");
        assert!(err.to_string().contains("idx_session_metadata_agent_alias"));
    }

    #[test]
    fn set_session_name_persists() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_name("s1", "My Session").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].name.as_deref(), Some("My Session"));
    }

    #[test]
    fn set_session_name_updates_existing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_name("s1", "First").unwrap();
        backend.set_session_name("s1", "Second").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta[0].name.as_deref(), Some("Second"));
    }

    #[test]
    fn sessions_without_name_return_none() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert_eq!(meta.len(), 1);
        assert!(meta[0].name.is_none());
    }

    // ── session state tests ─────────────────────────────────────────

    #[test]
    fn session_state_idle_to_running() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        let state = backend.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "running");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
        assert!(state.turn_started_at.is_some());
    }

    #[test]
    fn session_state_running_to_idle() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        backend.set_session_state("s1", "idle", None).unwrap();

        let state = backend.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "idle");
        assert!(state.turn_id.is_none());
        assert!(state.turn_started_at.is_none());
    }

    #[test]
    fn session_state_running_to_error() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        backend
            .set_session_state("s1", "error", Some("turn-1"))
            .unwrap();

        let state = backend.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "error");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn list_running_sessions_returns_running_only() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("a")).unwrap();
        backend.append("s2", &ChatMessage::user("b")).unwrap();
        backend.append("s3", &ChatMessage::user("c")).unwrap();

        backend
            .set_session_state("s1", "running", Some("t1"))
            .unwrap();
        backend
            .set_session_state("s2", "running", Some("t2"))
            .unwrap();
        // s3 stays idle (default)

        let running = backend.list_running_sessions();
        assert_eq!(running.len(), 2);
        let keys: Vec<&str> = running.iter().map(|m| m.key.as_str()).collect();
        assert!(keys.contains(&"s1"));
        assert!(keys.contains(&"s2"));
    }

    #[test]
    fn list_stuck_sessions_detects_old_running() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("a")).unwrap();

        // Manually set an old turn_started_at
        {
            let conn = backend.conn.lock();
            let old_time = (Utc::now() - Duration::seconds(600)).to_rfc3339();
            conn.execute(
                "UPDATE session_metadata SET state = 'running', turn_id = 'old', turn_started_at = ?1 WHERE session_key = 's1'",
                params![old_time],
            ).unwrap();
        }

        let stuck = backend.list_stuck_sessions(300); // 5 min threshold
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].key, "s1");

        // Not stuck if threshold is longer
        let not_stuck = backend.list_stuck_sessions(900); // 15 min threshold
        assert_eq!(not_stuck.len(), 0);
    }

    #[test]
    fn get_session_state_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        let state = backend.get_session_state("nonexistent").unwrap();
        assert!(state.is_none());
    }

    #[test]
    fn session_state_migration_preserves_data() {
        let tmp = TempDir::new().unwrap();
        // Create backend (runs migration)
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();

        // Re-open (migration should be idempotent)
        drop(backend);
        let backend2 = SqliteSessionBackend::new(tmp.path()).unwrap();
        let msgs = backend2.load("s1");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");

        // State should default to idle
        let state = backend2.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "idle");
    }

    #[test]
    fn schema_migration_repairs_individually_missing_state_columns() {
        let tmp = TempDir::new().unwrap();
        let sessions_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let db_path = sessions_dir.join("sessions.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_metadata (
                session_key TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                last_activity TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0,
                name TEXT,
                state TEXT NOT NULL DEFAULT 'idle'
            );",
        )
        .unwrap();
        drop(conn);

        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        drop(backend);
        let conn = Connection::open(db_path).unwrap();
        for column in ["turn_id", "turn_started_at"] {
            let present: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('session_metadata') \
                     WHERE name = ?1",
                    params![column],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(present, "missing repaired column {column}");
        }
    }

    #[test]
    fn empty_name_clears_to_none() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_name("s1", "Named").unwrap();
        backend.set_session_name("s1", "").unwrap();

        let meta = backend.list_sessions_with_metadata();
        assert!(meta[0].name.is_none());
    }

    // ── get_session_metadata tests ─────────────────────────────────

    #[test]
    fn get_session_metadata_returns_full_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.append("s1", &ChatMessage::assistant("hi")).unwrap();
        backend.set_session_name("s1", "My Chat").unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.key, "s1");
        assert_eq!(meta.name.as_deref(), Some("My Chat"));
        assert_eq!(meta.message_count, 2);
    }

    #[test]
    fn get_session_metadata_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert!(backend.get_session_metadata("nonexistent").is_none());
    }

    #[test]
    fn agent_alias_roundtrips_through_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend.set_session_agent_alias("s1", "scout").unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.agent_alias.as_deref(), Some("scout"));

        let listed = backend.list_sessions_with_metadata();
        let row = listed.iter().find(|m| m.key == "s1").unwrap();
        assert_eq!(row.agent_alias.as_deref(), Some("scout"));

        // Standalone getter also works.
        let alias = backend.get_session_agent_alias("s1").unwrap();
        assert_eq!(alias.as_deref(), Some("scout"));
    }

    #[test]
    fn rename_agent_attribution_repoints_only_matching_sessions() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
        backend.append("s1", &ChatMessage::user("hi")).unwrap();
        backend.set_session_agent_alias("s1", "scout").unwrap();
        backend.append("s2", &ChatMessage::user("yo")).unwrap();
        backend.set_session_agent_alias("s2", "other").unwrap();

        // Rename scout → ranger: the conversation history is kept and its
        // attribution follows the renamed agent (contrast clear, which NULLs it).
        let n = backend.rename_agent_attribution("scout", "ranger").unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            backend.get_session_agent_alias("s1").unwrap().as_deref(),
            Some("ranger")
        );
        // unrelated session untouched
        assert_eq!(
            backend.get_session_agent_alias("s2").unwrap().as_deref(),
            Some("other")
        );
        // unknown source → 0
        assert_eq!(backend.rename_agent_attribution("ghost", "x").unwrap(), 0);
    }

    #[test]
    fn agent_alias_set_before_any_append_upserts_metadata() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // No prior append — metadata row does not exist yet. UPSERT
        // path must still record the alias so the WS handshake can
        // attribute the session before the first user message lands.
        backend.set_session_agent_alias("s1", "scout").unwrap();

        let alias = backend.get_session_agent_alias("s1").unwrap();
        assert_eq!(alias.as_deref(), Some("scout"));
    }

    #[test]
    fn session_context_roundtrips_channel_room_sender() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend
            .set_session_context(
                "s1",
                SessionContext {
                    channel_id: Some("discord.clamps"),
                    room_id: Some("1234567890"),
                    sender_id: Some("@user:matrix"),
                },
            )
            .unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.channel_id.as_deref(), Some("discord.clamps"));
        assert_eq!(meta.room_id.as_deref(), Some("1234567890"));
        assert_eq!(meta.sender_id.as_deref(), Some("@user:matrix"));

        // Second call with partial context must NOT clear the columns
        // already filled in — set_session_context is additive.
        backend
            .set_session_context(
                "s1",
                SessionContext {
                    channel_id: None,
                    room_id: Some("1234567890"),
                    sender_id: None,
                },
            )
            .unwrap();
        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.channel_id.as_deref(), Some("discord.clamps"));
        assert_eq!(meta.sender_id.as_deref(), Some("@user:matrix"));
    }

    #[test]
    fn session_context_creates_metadata_row_before_first_append() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend
            .set_session_context(
                "s1",
                SessionContext {
                    channel_id: Some("telegram.production"),
                    room_id: None,
                    sender_id: Some("@alice"),
                },
            )
            .unwrap();

        let meta = backend.get_session_metadata("s1").unwrap();
        assert_eq!(meta.channel_id.as_deref(), Some("telegram.production"));
        assert_eq!(meta.sender_id.as_deref(), Some("@alice"));
        assert!(meta.room_id.is_none());
    }

    #[test]
    fn get_session_metadata_matches_list() {
        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        backend.append("s1", &ChatMessage::user("a")).unwrap();
        backend.append("s1", &ChatMessage::user("b")).unwrap();
        backend.append("s2", &ChatMessage::user("c")).unwrap();

        let single = backend.get_session_metadata("s1").unwrap();
        let all = backend.list_sessions_with_metadata();
        let from_list = all.iter().find(|m| m.key == "s1").unwrap();

        assert_eq!(single.message_count, from_list.message_count);
        assert_eq!(single.name, from_list.name);
        assert_eq!(single.created_at, from_list.created_at);
        assert_eq!(single.last_activity, from_list.last_activity);
    }
}
