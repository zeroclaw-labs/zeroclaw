//! JSONL-based session persistence for channel conversations.

use crate::session_backend::SessionBackend;
#[cfg(unix)]
use crate::session_backend::SessionEffectAuthorization;
use crate::session_disposition::jsonl::{self, Entry};
#[cfg(unix)]
use crate::session_disposition::{
    SessionDispositionBackend, SessionDispositionEffect, SessionDispositionIdentity, not_attempted,
};
use crate::session_disposition::{SessionDispositionObservation, SessionDispositionWitness};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use zeroclaw_api::model_provider::ChatMessage;
pub use zeroclaw_api::session_keys::sanitize_session_key;

#[derive(Default)]
pub(crate) struct MutationState {
    migrated: bool,
    receipt_state_uncertain: bool,
}

pub(crate) type MutationLock = parking_lot::Mutex<MutationState>;

// Release the OS lock before the process-local lock.
type MutationGuard<'a> = (File, parking_lot::MutexGuard<'a, MutationState>);

struct MutationLockRecord {
    lock: Weak<MutationLock>,
    migrated: bool,
}

static MUTATION_LOCKS: OnceLock<parking_lot::Mutex<HashMap<PathBuf, MutationLockRecord>>> =
    OnceLock::new();

/// Append-only JSONL session store for channel conversations.
pub struct SessionStore {
    sessions_dir: PathBuf,
    mutation_lock: Arc<MutationLock>,
}

impl SessionStore {
    /// Create a new session store, ensuring the sessions directory exists.
    pub fn new(workspace_dir: &Path) -> std::io::Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let mutation_lock = mutation_lock_for(&sessions_dir)?;
        {
            let mut state = mutation_lock.lock();
            let _directory = jsonl::lock_directory(&sessions_dir, true)?;
            refresh_migration_state(workspace_dir, &sessions_dir, &mut state)?;
        }
        Ok(Self {
            sessions_dir,
            mutation_lock,
        })
    }

    /// Compute the file path for a session key, sanitizing for filesystem safety.
    fn session_path(&self, session_key: &str) -> PathBuf {
        session_path(&self.sessions_dir, session_key)
    }

    /// Sidecar file recording whether `session_key`'s transcript currently
    /// starts with the synthetic trim breadcrumb. Kept as a canonical fact
    /// next to the transcript so restore never has to infer provenance from
    /// message text.
    fn trim_breadcrumb_path(&self, session_key: &str) -> PathBuf {
        trim_breadcrumb_path(&self.sessions_dir, session_key)
    }

    /// Persist whether the session's transcript starts with the synthetic
    /// trim breadcrumb. Respects the migration fence: fails if the JSONL
    /// store has been migrated to SQLite.
    pub fn set_trim_breadcrumb(&self, session_key: &str, present: bool) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        require_writable_session(&self.session_path(session_key))?;
        self.write_breadcrumb_unlocked(session_key, present)
    }

    fn write_breadcrumb_unlocked(&self, session_key: &str, present: bool) -> io::Result<()> {
        let mut staged =
            tempfile::Builder::new().make_in(&self.sessions_dir, create_staging_file)?;
        staged.write_all(if present { b"1" } else { b"0" })?;
        // Do not truncate through a sidecar symlink or hard link into a
        // different session's file; publication replaces only this name.
        publish_staged_rewrite(staged, &self.trim_breadcrumb_path(session_key))
    }

    /// Read the persisted breadcrumb flag. `None` if never recorded.
    ///
    /// Only an exact one-byte `b"0"` or `b"1"` is a verified reading; any
    /// other content (empty, truncated, extra bytes, a stray byte) is not
    /// something `set_trim_breadcrumb` ever wrote, so it is corruption, not a
    /// legitimate `false`. Treating it as `Some(false)` would let a restore
    /// mark an untrimmed transcript as trim-clean and skip persisting the
    /// correction. Callers must fail closed on this `Err`, the same as any
    /// other unreadable breadcrumb.
    pub fn get_trim_breadcrumb(&self, session_key: &str) -> std::io::Result<Option<bool>> {
        if matches!(
            classify_session(&self.session_path(session_key))?,
            Entry::Marker(_)
        ) {
            return Ok(None);
        }
        match jsonl::open_regular(&self.trim_breadcrumb_path(session_key), false) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(2).read_to_end(&mut bytes)?;
                match bytes.as_slice() {
                    [b'0'] => Ok(Some(false)),
                    [b'1'] => Ok(Some(true)),
                    _ => Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "corrupt trim breadcrumb for session {session_key}: expected a single \
                         b'0' or b'1' byte"
                        ),
                    )),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Load all messages for a session from its JSONL file.
    /// Returns an empty vec if the path is not a regular JSONL session file or
    /// the file is unreadable.
    pub fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(Entry::Transcript(file)) = classify_session(&self.session_path(session_key)) else {
            return Vec::new();
        };
        load_transcript(file)
    }

    /// Like `load`, but fails closed: a missing file returns `Ok(empty)`,
    /// while an unreadable file, a line-read error, or a JSON parse failure
    /// returns `Err` so callers can distinguish "no session" from "existing
    /// transcript that could not be verified" instead of seeding a
    /// new-message-only cache over an existing transcript.
    pub fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        match classify_session(&self.session_path(session_key))? {
            Entry::Missing | Entry::Marker(_) => Ok(Vec::new()),
            Entry::Transcript(file) => {
                let mut messages = Vec::new();
                read_strict_transcript(file, |message| messages.push(message))?;
                Ok(messages)
            }
        }
    }

    /// Append a message on its own JSONL line without modifying existing bytes.
    /// A non-empty file without a final LF gets one separator before the new record.
    /// A write error may leave a partial record; the error is returned without rollback.
    /// The next append still starts on a new line.
    pub fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        self.append_unlocked(session_key, message)
    }

    fn mutation_guard(&self) -> io::Result<MutationGuard<'_>> {
        let mut process = self.mutation_lock.lock();
        if process.migrated || process.receipt_state_uncertain {
            return Err(inactive_jsonl());
        }
        let directory = jsonl::lock_directory(&self.sessions_dir, false)?;
        // The OS lock serializes cooperating processes as well. An inactive
        // flag cached by one process can't fence a writer in another.
        let workspace_dir = self.sessions_dir.parent().unwrap_or_else(|| Path::new(""));
        refresh_migration_state(workspace_dir, &self.sessions_dir, &mut process)?;
        if process.migrated {
            return Err(inactive_jsonl());
        }
        Ok((directory, process))
    }

    fn append_unlocked(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let path = self.session_path(session_key);
        validate_jsonl_suffix(&path)?;
        let (mut file, needs_separator) = match jsonl::classify_append(&path)? {
            Entry::Transcript(mut file) => {
                let needs_separator = ends_without_lf(&mut file)?;
                (file, needs_separator)
            }
            // Exclusive creation guarantees an empty file; its handle is write-only.
            Entry::Missing => (jsonl::create_append(&path)?, false),
            Entry::Marker(_) => return Err(unacknowledged_delete()),
        };

        let mut record = Vec::new();
        if needs_separator {
            record.push(b'\n');
        }
        serde_json::to_writer(&mut record, message)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        record.push(b'\n');
        file.write_all(&record)
    }

    /// Remove the last message from a session's JSONL file.
    /// Rewrite approach: load all messages, drop the last, rewrite. This is
    /// O(n) but rollbacks are rare.
    pub fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        let Some(file) = open_writable_session(&self.session_path(session_key))? else {
            return Ok(false);
        };
        let mut messages = load_transcript(file);
        if messages.is_empty() {
            return Ok(false);
        }
        messages.pop();
        self.rewrite(session_key, &messages)?;
        Ok(true)
    }

    /// Replace the last message without exposing an intermediate truncated session.
    pub fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.update_last_with(session_key, message, publish_staged_rewrite)
    }

    fn update_last_with<F>(
        &self,
        session_key: &str,
        message: &ChatMessage,
        persist: F,
    ) -> std::io::Result<bool>
    where
        F: FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
    {
        let _guard = self.mutation_guard()?;
        let Some(file) = open_writable_session(&self.session_path(session_key))? else {
            return Ok(false);
        };
        let mut messages = load_transcript(file);
        let Some(last) = messages.last_mut() else {
            return Ok(false);
        };
        *last = message.clone();
        self.rewrite_with(session_key, &messages, persist)?;
        Ok(true)
    }

    /// Compact a session file by rewriting only valid messages (removes corrupt lines).
    pub fn compact(&self, session_key: &str) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        let messages = self.load(session_key);
        self.rewrite(session_key, &messages)
    }

    fn rewrite(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        self.rewrite_with(session_key, messages, publish_staged_rewrite)
    }

    /// Transcript+sidecar replacement assuming the caller already holds the
    /// mutation guard. Split out so the existence-guarded
    /// `replace_conversation_state_if_exists` can probe-then-write under one
    /// guard acquisition (`parking_lot` mutexes are not reentrant, so the
    /// trait methods cannot call each other while holding it).
    fn replace_locked(
        &self,
        _guard: &MutationGuard<'_>,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<()> {
        let previous_messages = self.load(session_key);
        self.rewrite(session_key, messages)?;
        let breadcrumb_write = self.write_breadcrumb_unlocked(session_key, breadcrumb_present);
        if let Err(e) = breadcrumb_write {
            // Best-effort: if this rewrite also fails, the transcript is left
            // at the new value with the stale flag, and the caller must
            // reconcile by reloading both files rather than trusting either
            // write succeeded.
            let _ = self.rewrite(session_key, &previous_messages);
            return Err(e);
        }
        Ok(())
    }

    fn rewrite_with<F>(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        persist: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
    {
        let path = self.session_path(session_key);
        require_writable_session(&path)?;
        let mut temp = tempfile::Builder::new().make_in(&self.sessions_dir, create_staging_file)?;
        for msg in messages {
            serde_json::to_writer(&mut temp, msg)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            temp.write_all(b"\n")?;
        }

        temp.as_file().sync_all()?;
        persist(temp, &path)
    }

    /// Clear all messages from a session by truncating its JSONL file.
    /// The file is preserved (empty) so the session key remains in `list_sessions`.
    /// Also removes the breadcrumb sidecar: a reset session has no synthetic
    /// marker, so a stale recorded `true` must not survive into the next
    /// first message and be mistaken for a real trim. The sidecar is removed
    /// even when the transcript is absent or not a regular JSONL file: it is
    /// independently creatable by `set_trim_breadcrumb`, so a sidecar-only
    /// state (e.g. after a transcript write failure or a prior cleanup that
    /// removed the transcript but not its provenance file) must not survive
    /// a `clear_messages` call and be misread by a later restore.
    pub fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let _guard = self.mutation_guard()?;
        let file = open_writable_session_legacy(&self.session_path(session_key))?;
        let _ = std::fs::remove_file(self.trim_breadcrumb_path(session_key));
        let Some(file) = file else {
            return Ok(0);
        };
        let count = load_transcript(file).len();
        if count > 0 {
            self.rewrite(session_key, &[])?;
        }
        Ok(count)
    }

    /// Delete a session's JSONL file and its breadcrumb sidecar. Returns
    /// `true` if either file existed. The sidecar is removed even when the
    /// transcript file is already absent (or not a regular JSONL session
    /// file) so stale provenance cannot affect a later session that reuses
    /// the same key.
    pub fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        self.delete_session_unlocked(session_key)
    }

    fn delete_session_unlocked(&self, session_key: &str) -> std::io::Result<bool> {
        let path = self.session_path(session_key);
        let crumb_path = self.trim_breadcrumb_path(session_key);
        if open_writable_session_legacy(&path)?.is_none() {
            let had_crumb = crumb_path.exists();
            let _ = std::fs::remove_file(&crumb_path);
            return Ok(had_crumb);
        }
        std::fs::remove_file(&path)?;
        let _ = std::fs::remove_file(crumb_path);
        Ok(true)
    }

    /// Return the modification time of a regular session JSONL file.
    pub fn session_mtime(&self, session_key: &str) -> Option<std::time::SystemTime> {
        if let Ok(Entry::Transcript(file)) = classify_session(&self.session_path(session_key)) {
            file.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        } else {
            None
        }
    }

    /// List all session keys that have regular JSONL files on disk.
    pub fn list_sessions(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !is_regular_jsonl_session_file(&entry.path()) {
                    return None;
                }
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(".jsonl").map(String::from)
            })
            .collect()
    }

    #[cfg(unix)]
    fn delete_with_disposition_using<P, S>(
        &self,
        witness: &SessionDispositionWitness,
        expected_owner: Option<&str>,
        authorize: &SessionEffectAuthorization<'_>,
        publish: P,
        sync_published: S,
    ) -> io::Result<bool>
    where
        P: FnOnce(jsonl::Staged<'_>, &str) -> io::Result<()>,
        S: FnOnce(&jsonl::Directory) -> io::Result<()>,
    {
        let guard = self.mutation_guard()?;
        let _authority = authorize(None)?;
        if expected_owner.is_some() {
            return Ok(false);
        }
        let bytes = jsonl::encode(witness)?;
        let directory = jsonl::Directory::for_lock(&self.sessions_dir, &guard.0)?;
        if directory.read_identity()?.as_ref() != Some(&witness.identity) {
            return Err(io::Error::other("JSONL backend instance changed"));
        }
        let name = session_filename(&witness.backend_key);
        let file = match directory.classify(&name)? {
            Entry::Missing => return Ok(false),
            Entry::Marker(_) => return Err(unacknowledged_delete()),
            Entry::Transcript(file) => file,
        };
        read_strict_transcript(file, |_| {}).map_err(not_attempted)?;

        // Verify support for directory sync before changing either old fact.
        directory.sync()?;
        let staged = directory.stage(&bytes)?;
        // Once rename is attempted, an error does not prove no effect. The
        // transition owner must resolve it using the exact marker; never
        // roll back by seeding a transcript or unlinking the destination.
        publish(staged, &name)?;
        sync_published(&directory)?;
        Ok(true)
    }

    /// Inspect an original JSONL backend even if SQLite is now configured.
    /// The probe never creates the directory, lock, identity or a backend.
    #[cfg(unix)]
    pub fn observe_original_session_disposition(
        workspace_dir: &Path,
        witness: &SessionDispositionWitness,
    ) -> io::Result<SessionDispositionObservation> {
        use SessionDispositionObservation::{Committed, Indeterminate, NotCommitted};
        let observed = with_original_disposition(workspace_dir, witness, |directory| {
            match directory.classify(&session_filename(&witness.backend_key))? {
                Entry::Marker(actual) if actual == *witness => {
                    // The original writer could have died or reported an
                    // error after publishing but before syncing the rename.
                    directory.sync()?;
                    Ok(Committed)
                }
                Entry::Transcript(file) => {
                    read_strict_transcript(file, |_| {})?;
                    Ok(NotCommitted)
                }
                Entry::Marker(_) | Entry::Missing => Ok(Indeterminate),
            }
        })?;
        Ok(observed.unwrap_or(Indeterminate))
    }

    /// Unsupported platforms cannot establish a crash-durable JSONL witness.
    #[cfg(not(unix))]
    pub fn observe_original_session_disposition(
        _workspace_dir: &Path,
        _witness: &SessionDispositionWitness,
    ) -> io::Result<SessionDispositionObservation> {
        Ok(SessionDispositionObservation::Indeterminate)
    }

    /// Remove only the marker belonging to a disposition already recorded as
    /// committed in the task control plane. Does not activate JSONL.
    #[cfg(unix)]
    pub fn acknowledge_original_session_disposition(
        workspace_dir: &Path,
        witness: &SessionDispositionWitness,
    ) -> io::Result<()> {
        with_original_disposition(workspace_dir, witness, |directory| {
            let name = session_filename(&witness.backend_key);
            match directory.classify(&name)? {
                Entry::Marker(actual) if actual == *witness => {
                    let crumb = trim_breadcrumb_filename(&witness.backend_key);
                    match directory.remove(&crumb) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                    // Make cleanup durable while reuse is still excluded.
                    directory.sync()?;
                    directory.remove(&name)?;
                    directory.sync()
                }
                // A concurrent/retried acknowledgment must not touch a new
                // session's transcript or independently created breadcrumb.
                Entry::Missing | Entry::Transcript(_) => directory.sync(),
                Entry::Marker(_) => Err(unacknowledged_delete()),
            }
        })?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "original JSONL backend unavailable",
            )
        })
    }

    /// Unsupported platforms cannot safely remove a crash-durable JSONL witness.
    #[cfg(not(unix))]
    pub fn acknowledge_original_session_disposition(
        _workspace_dir: &Path,
        _witness: &SessionDispositionWitness,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "crash-durable JSONL disposition is unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn with_original_disposition<T>(
    workspace_dir: &Path,
    witness: &SessionDispositionWitness,
    effect: impl FnOnce(&jsonl::Directory) -> io::Result<T>,
) -> io::Result<Option<T>> {
    if witness.identity.backend != SessionDispositionBackend::ChatJsonl
        || witness.effect != SessionDispositionEffect::Delete
        || witness.backend_key.is_empty()
        || witness.operation_id.is_empty()
    {
        return Ok(None);
    }
    let dir = workspace_dir.join("sessions");
    let process = match mutation_lock_for(&dir) {
        Ok(process) => process,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let _process = process.lock();
    let lock = match jsonl::lock_directory(&dir, false) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let directory = jsonl::Directory::for_lock(&dir, &lock)?;
    if directory.read_identity()?.as_ref() != Some(&witness.identity) {
        return Ok(None);
    }
    effect(&directory).map(Some)
}

fn session_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(session_filename(key))
}

fn trim_breadcrumb_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(trim_breadcrumb_filename(key))
}

fn session_filename(key: &str) -> String {
    format!("{}.jsonl", sanitize_session_key(key))
}

fn trim_breadcrumb_filename(key: &str) -> String {
    format!("{}.trim_breadcrumb", sanitize_session_key(key))
}

// Read the final byte through the already-readable classified handle.
fn ends_without_lf(file: &mut File) -> io::Result<bool> {
    let len = file.seek(SeekFrom::End(0))?;
    if len == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

fn load_transcript(file: File) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    for line in BufReader::new(file).lines() {
        if let Ok(line) = line
            && let Ok(message) = serde_json::from_str(line.trim())
        {
            messages.push(message);
        }
    }
    messages
}

fn read_strict_transcript(file: File, mut accept: impl FnMut(ChatMessage)) -> io::Result<()> {
    for line in BufReader::new(file).lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let message = serde_json::from_str(trimmed).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("corrupt session JSONL line: {error}"),
            )
        })?;
        accept(message);
    }
    Ok(())
}

fn refresh_migration_state(
    workspace_dir: &Path,
    sessions_dir: &Path,
    state: &mut MutationState,
) -> io::Result<()> {
    match crate::session_sqlite::has_committed_jsonl_import_receipts(workspace_dir) {
        Ok(true) => mark_session_directory_migrated(sessions_dir, state),
        Ok(false) => {
            state.receipt_state_uncertain = false;
            Ok(())
        }
        Err(error) => {
            state.receipt_state_uncertain = true;
            Err(io::Error::other(format!(
                "Failed to inspect durable JSONL migration state: {error:#}"
            )))
        }
    }
}

fn inactive_jsonl() -> io::Error {
    io::Error::other("JSONL session store is inactive after SQLite migration")
}

fn unacknowledged_delete() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "JSONL session deletion has not been acknowledged",
    )
}

/// Rename a staged rewrite over the live session file.
///
/// Readers take no lock, so one may hold the session file open at this
/// moment, and that must not stop the replace on any platform. On Windows
/// `persist` is a bare `MoveFileExW`, which cannot replace a file that any
/// reader holds open. `std::fs::rename` retries that refusal with POSIX
/// semantics, which swap the name while open readers keep the old contents,
/// as long as they opened it with delete sharing (std's own opens do). On Unix
/// both are rename(2).
fn publish_staged_rewrite(temp: tempfile::NamedTempFile, path: &Path) -> std::io::Result<()> {
    let mut staged = temp.into_temp_path();
    std::fs::rename(&staged, path)?;
    // The staging name no longer exists, so there is nothing left to clean up.
    staged.disable_cleanup(true);
    Ok(())
}

/// Create a staging file the way `NamedTempFile::new_in` does (exclusively,
/// owner-only on Unix) but without the Windows temporary attribute: `persist`
/// would clear that attribute, and the plain rename that publishes the file
/// would carry it onto the session file.
fn create_staging_file(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn is_regular_jsonl_session_file(path: &Path) -> bool {
    matches!(classify_session(path), Ok(Entry::Transcript(_)))
}

fn validate_jsonl_suffix(path: &Path) -> io::Result<()> {
    if path
        .extension()
        .is_none_or(|extension| extension != "jsonl")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path must have a .jsonl extension",
        ));
    }

    Ok(())
}

fn classify_session(path: &Path) -> io::Result<Entry> {
    validate_jsonl_suffix(path)?;
    jsonl::classify(path)
}

fn open_writable_session(path: &Path) -> io::Result<Option<File>> {
    match classify_session(path)? {
        Entry::Missing => Ok(None),
        Entry::Transcript(file) => Ok(Some(file)),
        Entry::Marker(_) => Err(unacknowledged_delete()),
    }
}

// Legacy reset and delete may clean a dangling breadcrumb without touching
// an invalid transcript path. Tombstones (including malformed ones) are not
// a dangling path: they must still protect their breadcrumb until ack.
fn open_writable_session_legacy(path: &Path) -> io::Result<Option<File>> {
    match open_writable_session(path) {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(None),
        other => other,
    }
}

fn require_writable_session(path: &Path) -> io::Result<()> {
    open_writable_session(path).map(drop)
}

pub(crate) fn mutation_lock_for(sessions_dir: &Path) -> std::io::Result<Arc<MutationLock>> {
    let key = sessions_dir.canonicalize()?;
    let registry = MUTATION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    let mut locks = registry.lock();
    locks.retain(|_, record| record.migrated || record.lock.strong_count() > 0);

    if let Some(lock) = locks.get(&key).and_then(|record| record.lock.upgrade()) {
        return Ok(lock);
    }

    let migrated = locks.get(&key).is_some_and(|record| record.migrated);
    let lock = Arc::new(MutationLock::new(MutationState {
        migrated,
        receipt_state_uncertain: false,
    }));
    locks.insert(
        key,
        MutationLockRecord {
            lock: Arc::downgrade(&lock),
            migrated,
        },
    );
    Ok(lock)
}

pub(crate) fn mark_session_directory_migrated(
    sessions_dir: &Path,
    state: &mut MutationState,
) -> std::io::Result<()> {
    state.migrated = true;
    state.receipt_state_uncertain = false;
    let key = sessions_dir.canonicalize()?;
    let registry = MUTATION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    if let Some(record) = registry.lock().get_mut(&key) {
        record.migrated = true;
    }
    Ok(())
}

pub(crate) fn mark_session_directory_receipt_state_uncertain(state: &mut MutationState) {
    state.receipt_state_uncertain = true;
}

pub(crate) fn clear_session_directory_receipt_state_uncertain(state: &mut MutationState) {
    state.receipt_state_uncertain = false;
}

#[cfg(test)]
pub(crate) fn forget_session_directory_migration_state_for_test(
    sessions_dir: &Path,
) -> std::io::Result<()> {
    let key = sessions_dir.canonicalize()?;
    if let Some(registry) = MUTATION_LOCKS.get() {
        registry.lock().remove(&key);
    }
    Ok(())
}

impl SessionBackend for SessionStore {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        self.load(session_key)
    }

    fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        self.try_load(session_key)
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.append(session_key, message)
    }

    fn append_authorized(
        &self,
        session_key: &str,
        message: &ChatMessage,
        authorize: &crate::session_backend::SessionEffectAuthorization<'_>,
        committed: &mut dyn FnMut(),
    ) -> std::io::Result<usize> {
        let _guard = self.mutation_guard()?;
        if !is_regular_jsonl_session_file(&self.session_path(session_key)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Session not found",
            ));
        }
        let _authority = authorize(None)?;
        let count = self.try_load(session_key)?.len() + 1;
        self.append_unlocked(session_key, message)?;
        committed();
        Ok(count)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.remove_last(session_key)
    }

    fn rewrite_messages(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        self.rewrite(session_key, messages)
    }

    fn set_session_trim_breadcrumb(&self, session_key: &str, present: bool) -> std::io::Result<()> {
        self.set_trim_breadcrumb(session_key, present)
    }

    fn replace_conversation_state(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<()> {
        // Hold the migration fence across both writes so a concurrent
        // migration cannot interleave, and the transcript+flag pair is not
        // observed partially. Two separate files still can't be made
        // crash-atomic, so on a breadcrumb-write failure this rolls the
        // transcript back to its pre-replace content instead of leaving a
        // new transcript paired with a stale flag — a process that dies
        // between the writes can still leave the pair split, but an
        // in-process failure converges back to the last known-good state.
        let guard = self.mutation_guard()?;
        self.replace_locked(&guard, session_key, messages, breadcrumb_present)
    }

    fn replace_conversation_state_if_exists(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<bool> {
        // Hold the mutation guard across the existence probe and both file
        // writes. `delete_session` holds the same guard, so a deleter cannot
        // commit between the probe and the writes: once deletion wins, the
        // post-turn write is a no-op instead of recreating the transcript
        // and sidecar files the delete just removed.
        let guard = self.mutation_guard()?;
        if !is_regular_jsonl_session_file(&self.session_path(session_key)) {
            return Ok(false);
        }
        self.replace_locked(&guard, session_key, messages, breadcrumb_present)?;
        Ok(true)
    }

    fn get_session_trim_breadcrumb(&self, session_key: &str) -> std::io::Result<Option<bool>> {
        self.get_trim_breadcrumb(session_key)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.update_last(session_key, message)
    }

    fn list_sessions(&self) -> Vec<String> {
        self.list_sessions()
    }

    fn list_sessions_with_metadata(&self) -> Vec<crate::session_backend::SessionMetadata> {
        use chrono::{DateTime, Utc};
        self.list_sessions()
            .into_iter()
            .filter_map(|key| {
                let last_activity = self.session_mtime(&key).map(DateTime::<Utc>::from)?;
                Some(crate::session_backend::SessionMetadata {
                    name: None,
                    created_at: last_activity,
                    last_activity,
                    message_count: 0,
                    key,
                    agent_alias: None,
                    channel_id: None,
                    room_id: None,
                    sender_id: None,
                    principal_id: None,
                })
            })
            .collect()
    }

    fn compact(&self, session_key: &str) -> std::io::Result<()> {
        self.compact(session_key)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        self.clear_messages(session_key)
    }

    fn with_session_owner(
        &self,
        _session_key: &str,
        effect: &mut dyn FnMut(Option<&str>) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        effect(None)
    }

    fn delete_session_authorized(
        &self,
        session_key: &str,
        expected_owner: Option<&str>,
        authorize: &crate::session_backend::SessionEffectAuthorization<'_>,
    ) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        let _authority = authorize(None)?;
        if expected_owner.is_some() {
            return Ok(false);
        }
        self.delete_session_unlocked(session_key)
    }

    #[cfg(unix)]
    fn session_disposition_identity(&self) -> io::Result<SessionDispositionIdentity> {
        let guard = self.mutation_guard()?;
        jsonl::Directory::for_lock(&self.sessions_dir, &guard.0)?.install_identity()
    }

    #[cfg(unix)]
    fn delete_session_with_disposition(
        &self,
        witness: &SessionDispositionWitness,
        expected_owner: Option<&str>,
        authorize: &SessionEffectAuthorization<'_>,
    ) -> io::Result<bool> {
        self.delete_with_disposition_using(
            witness,
            expected_owner,
            authorize,
            |staged, target| staged.replace(target),
            jsonl::Directory::sync,
        )
    }

    fn observe_session_disposition(
        &self,
        witness: &SessionDispositionWitness,
    ) -> io::Result<SessionDispositionObservation> {
        let workspace_dir = self.sessions_dir.parent().unwrap_or_else(|| Path::new(""));
        Self::observe_original_session_disposition(workspace_dir, witness)
    }

    fn acknowledge_session_disposition(
        &self,
        witness: &SessionDispositionWitness,
    ) -> io::Result<()> {
        let workspace_dir = self.sessions_dir.parent().unwrap_or_else(|| Path::new(""));
        Self::acknowledge_original_session_disposition(workspace_dir, witness)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        self.delete_session(session_key)
    }

    /// Quick existence probe using the same regular-file policy as the other
    /// JSONL session operations.
    fn session_exists(&self, session_key: &str) -> bool {
        is_regular_jsonl_session_file(&self.session_path(session_key))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn guarded_jsonl_append_requires_existing_authorized_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        store.append("s", &ChatMessage::user("before")).unwrap();
        let called = std::cell::Cell::new(false);
        let denied = backend.append_authorized(
            "s",
            &ChatMessage::assistant("denied"),
            &|owner| {
                assert_eq!(owner, None);
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "revoked",
                ))
            },
            &mut || called.set(true),
        );
        assert_eq!(
            denied.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(!called.get());
        assert_eq!(store.load("s").len(), 1);
        let count = backend
            .append_authorized(
                "s",
                &ChatMessage::assistant("allowed"),
                &|owner| {
                    assert_eq!(owner, None);
                    Ok(Box::new(()))
                },
                &mut || called.set(true),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert!(called.get());
        assert_eq!(store.load("s").len(), 2);
        backend.delete_session("s").unwrap();
        assert_eq!(
            backend
                .append_authorized(
                    "s",
                    &ChatMessage::assistant("recreated"),
                    &|_| panic!("missing row cannot be authorized"),
                    &mut || panic!("missing row cannot publish effects"),
                )
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(!backend.session_exists("s"));
    }

    #[test]
    fn no_state_backend_admits_locked_live_owner_without_persisting_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        // Named admin creators may keep a live owner even though JSONL cannot
        // persist attribution; scoped creation already refuses this backend.
        assert!(backend.set_session_principal("s", "user:alice").is_err());
        let calls = std::cell::Cell::new(0);
        backend
            .set_session_state_authorized(
                "s",
                "running",
                Some("turn"),
                Some("user:alice"),
                &|owner| {
                    assert_eq!(owner, Some("user:alice"));
                    calls.set(calls.get() + 1);
                    Ok(Box::new(()))
                },
            )
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert!(backend.get_session_state("s").unwrap().is_none());
        assert!(
            backend
                .set_session_state_authorized(
                    "s",
                    "running",
                    Some("turn"),
                    Some("user:alice"),
                    &|_| Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "revoked"
                    ))
                )
                .is_err()
        );
    }

    use super::*;
    use std::path::Path;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tempfile::TempDir;

    #[cfg(unix)]
    fn witness(store: &SessionStore, key: &str) -> SessionDispositionWitness {
        SessionDispositionWitness {
            operation_id: "operation-1".to_owned(),
            effect: crate::session_disposition::SessionDispositionEffect::Delete,
            backend_key: key.to_owned(),
            identity: store.session_disposition_identity().unwrap(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_reopen_guards_legacy_writers_and_ack_retry() {
        use SessionDispositionObservation::{Committed, Indeterminate, NotCommitted};
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "chat:user";
        let alias = "chat/user";
        let old = ChatMessage::user("private old transcript");
        store.append(key, &old).unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        let requested = witness(&store, key);
        assert_eq!(
            store.observe_session_disposition(&requested).unwrap(),
            NotCommitted
        );

        assert!(
            store
                .delete_session_with_disposition(&requested, None, &|owner| {
                    assert_eq!(owner, None);
                    Ok(Box::new(()))
                })
                .unwrap()
        );
        drop(store);

        let reopened = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &reopened;
        assert_eq!(
            backend.session_disposition_identity().unwrap(),
            requested.identity
        );
        assert_eq!(
            backend.observe_session_disposition(&requested).unwrap(),
            Committed
        );
        let marker_path = reopened.session_path(key);
        let marker = std::fs::read(&marker_path).unwrap();
        assert_eq!(marker, jsonl::encode(&requested).unwrap());
        assert_eq!(
            std::fs::metadata(&marker_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(backend.load(alias).is_empty());
        assert!(backend.try_load(alias).unwrap().is_empty());
        assert!(!backend.session_exists(alias));
        assert!(backend.list_sessions().is_empty());
        assert!(backend.list_sessions_with_metadata().is_empty());
        assert!(reopened.session_mtime(alias).is_none());
        assert!(
            backend
                .get_session_trim_breadcrumb(alias)
                .unwrap()
                .is_none()
        );
        assert!(
            !backend
                .replace_conversation_state_if_exists(alias, &[], false)
                .unwrap()
        );
        for error in [
            backend.append(alias, &old).unwrap_err(),
            backend
                .set_session_trim_breadcrumb(alias, false)
                .unwrap_err(),
            backend.rewrite_messages(alias, &[]).unwrap_err(),
            backend
                .replace_conversation_state(alias, &[], false)
                .unwrap_err(),
            backend.compact(alias).unwrap_err(),
            backend.clear_messages(alias).unwrap_err(),
            backend.remove_last(alias).unwrap_err(),
            backend.update_last(alias, &old).unwrap_err(),
            backend.delete_session(alias).unwrap_err(),
            backend
                .delete_session_authorized(alias, None, &|_| Ok(Box::new(())))
                .unwrap_err(),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        }
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);
        assert_eq!(
            std::fs::read(reopened.trim_breadcrumb_path(alias)).unwrap(),
            b"1"
        );

        let mut foreign = requested.clone();
        foreign.operation_id.push_str("-foreign");
        assert_eq!(
            backend.observe_session_disposition(&foreign).unwrap(),
            Indeterminate
        );
        assert!(backend.acknowledge_session_disposition(&foreign).is_err());
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);

        backend.acknowledge_session_disposition(&requested).unwrap();
        assert!(!marker_path.exists());
        assert!(!reopened.trim_breadcrumb_path(alias).exists());
        let new = ChatMessage::user("new-session");
        backend.append(alias, &new).unwrap();
        backend.set_session_trim_breadcrumb(alias, false).unwrap();
        backend.acknowledge_session_disposition(&requested).unwrap();
        assert_eq!(backend.try_load(alias).unwrap()[0].content, new.content);
        assert_eq!(
            backend.get_session_trim_breadcrumb(alias).unwrap(),
            Some(false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_observation_needs_strict_transcript_for_non_commit() {
        use SessionDispositionObservation::{Committed, Indeterminate, NotCommitted};

        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "strict-observation";
        let requested = witness(&store, key);
        let transcript = store.session_path(key);
        let observe = || SessionStore::observe_original_session_disposition(tmp.path(), &requested);
        store
            .rewrite(
                key,
                &[
                    ChatMessage::user("question"),
                    ChatMessage::assistant("answer"),
                ],
            )
            .unwrap();
        let valid = std::fs::read(&transcript).unwrap();
        assert_eq!(observe().unwrap(), NotCommitted);
        std::fs::write(&transcript, b"").unwrap();
        assert_eq!(observe().unwrap(), NotCommitted);

        let marker = jsonl::encode(&requested).unwrap();
        let corruptions = [
            b"corrupt ordinary JSONL\n".to_vec(),
            vec![0xff, 0xfe, b'\n'],
            [valid.as_slice(), b"{\"role\":"].concat(),
            [&[1], &marker[1..]].concat(),
            [b"{", &marker[1..]].concat(),
            b"\0ZC-SESSION-DISPOSITION".to_vec(),
        ];
        for bytes in corruptions {
            std::fs::write(&transcript, &bytes).unwrap();
            assert!(observe().is_err(), "unexpected proof from {bytes:?}");
            assert_eq!(std::fs::read(&transcript).unwrap(), bytes);
        }

        std::fs::write(&transcript, marker).unwrap();
        assert_eq!(observe().unwrap(), Committed);
        let foreign = SessionDispositionWitness {
            operation_id: "different-operation".into(),
            ..requested.clone()
        };
        std::fs::write(&transcript, jsonl::encode(&foreign).unwrap()).unwrap();
        assert_eq!(observe().unwrap(), Indeterminate);
        std::fs::remove_file(&transcript).unwrap();
        assert_eq!(observe().unwrap(), Indeterminate);

        std::fs::rename(tmp.path().join("sessions"), tmp.path().join("original")).unwrap();
        let replacement = SessionStore::new(tmp.path()).unwrap();
        assert_ne!(
            replacement.session_disposition_identity().unwrap(),
            requested.identity
        );
        replacement
            .append(key, &ChatMessage::user("replacement"))
            .unwrap();
        assert_eq!(observe().unwrap(), Indeterminate);
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_strict_refusal_preserves_transcript_and_breadcrumb() {
        use crate::session_disposition::SessionDispositionNotAttempted;

        let is_not_attempted = |error: &io::Error| {
            error
                .get_ref()
                .is_some_and(|inner| inner.is::<SessionDispositionNotAttempted>())
        };
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "strict-refusal";
        let requested = witness(&store, key);
        let transcript = store.session_path(key);
        let breadcrumb = store.trim_breadcrumb_path(key);
        store.set_trim_breadcrumb(key, true).unwrap();
        let mut damaged_marker = jsonl::encode(&requested).unwrap();
        damaged_marker[0] = 1;
        let authorize: &SessionEffectAuthorization<'_> = &|_| Ok(Box::new(()));
        for bytes in [b"corrupt ordinary JSONL\n".as_slice(), &damaged_marker] {
            std::fs::write(&transcript, bytes).unwrap();
            let strict = store.try_load(key).unwrap_err();
            let refused = store
                .delete_session_with_disposition(&requested, None, authorize)
                .unwrap_err();
            assert!(is_not_attempted(&refused), "{refused:?}");
            assert_eq!(refused.kind(), strict.kind());
            assert_eq!(refused.to_string(), strict.to_string());
            assert_eq!(std::fs::read(&transcript).unwrap(), bytes);
            assert_eq!(std::fs::read(&breadcrumb).unwrap(), b"1");
        }

        store.rewrite(key, &[ChatMessage::user("valid")]).unwrap();
        let valid = std::fs::read(&transcript).unwrap();
        let rename_failure = store
            .delete_with_disposition_using(
                &requested,
                None,
                authorize,
                |_, _| Err(io::Error::other("injected rename failure")),
                |_| panic!("cannot sync after rename failure"),
            )
            .unwrap_err();
        assert!(!is_not_attempted(&rename_failure));
        assert_eq!(rename_failure.to_string(), "injected rename failure");

        #[derive(Debug)]
        struct Denied;
        impl std::fmt::Display for Denied {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("fixture authorization refused")
            }
        }
        impl std::error::Error for Denied {}
        let denial = store
            .delete_session_with_disposition(&requested, None, &|_| {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, Denied))
            })
            .unwrap_err();
        assert!(!is_not_attempted(&denial));
        assert!(denial.get_ref().is_some_and(|inner| inner.is::<Denied>()));
        assert_eq!(denial.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&transcript).unwrap(), valid);
        assert_eq!(std::fs::read(&breadcrumb).unwrap(), b"1");

        assert!(
            store
                .delete_session_with_disposition(&requested, None, authorize)
                .unwrap()
        );
        assert_eq!(
            std::fs::read(&transcript).unwrap(),
            jsonl::encode(&requested).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_faults_resolve_from_original_facts() {
        use SessionDispositionObservation::{Committed, Indeterminate, NotCommitted};
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "fault-control";
        let requested = witness(&store, key);
        let authorize: &SessionEffectAuthorization<'_> = &|_| Ok(Box::new(()));
        assert!(
            !store
                .delete_session_with_disposition(&requested, None, authorize)
                .unwrap()
        );
        assert_eq!(
            store.observe_session_disposition(&requested).unwrap(),
            Indeterminate
        );

        let transcript = store.session_path(key);
        let breadcrumb = store.trim_breadcrumb_path(key);
        std::fs::write(&transcript, b"corrupt ordinary JSONL\n").unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        assert!(
            store
                .delete_session_with_disposition(&requested, None, authorize)
                .is_err()
        );
        assert_eq!(
            std::fs::read(&transcript).unwrap(),
            b"corrupt ordinary JSONL\n"
        );
        assert_eq!(std::fs::read(&breadcrumb).unwrap(), b"1");
        store
            .rewrite(key, &[ChatMessage::user("original")])
            .unwrap();
        let old = std::fs::read(&transcript).unwrap();

        let pre = store.delete_with_disposition_using(
            &requested,
            None,
            authorize,
            |_, _| Err(io::Error::other("injected before rename")),
            |_| panic!("must not sync after failed publication"),
        );
        assert!(pre.is_err());
        assert_eq!(std::fs::read(&transcript).unwrap(), old);
        assert_eq!(std::fs::read(&breadcrumb).unwrap(), b"1");
        assert_eq!(
            store.observe_session_disposition(&requested).unwrap(),
            NotCommitted
        );

        let post = store.delete_with_disposition_using(
            &requested,
            None,
            authorize,
            |staged, target| staged.replace(target),
            |_| Err(io::Error::other("injected after rename")),
        );
        assert!(post.is_err());
        assert_eq!(std::fs::read(&breadcrumb).unwrap(), b"1");
        drop(store);
        assert_eq!(
            SessionStore::observe_original_session_disposition(tmp.path(), &requested).unwrap(),
            Committed
        );

        // A still-held name is essential while cleanup is failing.
        std::fs::remove_file(&breadcrumb).unwrap();
        std::fs::create_dir(&breadcrumb).unwrap();
        assert!(
            SessionStore::acknowledge_original_session_disposition(tmp.path(), &requested).is_err()
        );
        assert!(transcript.exists());
        std::fs::remove_dir(&breadcrumb).unwrap();
        SessionStore::acknowledge_original_session_disposition(tmp.path(), &requested).unwrap();
        assert!(!transcript.exists());
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_replace_is_bound_to_the_open_original_directory() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let replacement_tmp = TempDir::new().unwrap();
        let replacement = SessionStore::new(replacement_tmp.path()).unwrap();
        let key = "same-key";
        store.append(key, &ChatMessage::user("original")).unwrap();
        replacement
            .append(key, &ChatMessage::user("replacement"))
            .unwrap();
        replacement.set_trim_breadcrumb(key, true).unwrap();
        let requested = witness(&store, key);
        let replacement_bytes = std::fs::read(replacement.session_path(key)).unwrap();
        let detached = tmp.path().join("detached");
        std::fs::create_dir(&detached).unwrap();

        assert!(
            store
                .delete_with_disposition_using(
                    &requested,
                    None,
                    &|_| Ok(Box::new(())),
                    |staged, target| {
                        std::fs::rename(&store.sessions_dir, detached.join("sessions"))?;
                        std::fs::rename(&replacement.sessions_dir, &store.sessions_dir)?;
                        staged.replace(target)
                    },
                    jsonl::Directory::sync,
                )
                .unwrap()
        );
        assert_eq!(
            std::fs::read(store.session_path(key)).unwrap(),
            replacement_bytes
        );
        assert_eq!(
            std::fs::read(store.trim_breadcrumb_path(key)).unwrap(),
            b"1"
        );
        assert_eq!(
            SessionStore::observe_original_session_disposition(tmp.path(), &requested).unwrap(),
            SessionDispositionObservation::Indeterminate
        );
        assert_eq!(
            SessionStore::observe_original_session_disposition(&detached, &requested).unwrap(),
            SessionDispositionObservation::Committed
        );
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_migration_skips_marker_but_can_probe_inactive_backend() {
        use crate::session_sqlite::SqliteSessionBackend;
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "deleted";
        store.append(key, &ChatMessage::user("deleted")).unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        store
            .append("remaining", &ChatMessage::user("preserved"))
            .unwrap();
        let requested = witness(&store, key);
        assert!(
            store
                .delete_session_with_disposition(&requested, None, &|_| Ok(Box::new(())))
                .unwrap()
        );
        let sqlite = SqliteSessionBackend::new(tmp.path()).unwrap();
        assert_eq!(sqlite.migrate_from_jsonl(tmp.path()).unwrap(), 1);
        assert!(!sqlite.session_exists(key));
        assert!(sqlite.session_exists("remaining"));
        assert!(store.append("inactive", &ChatMessage::user("no")).is_err());
        assert_eq!(
            SessionStore::observe_original_session_disposition(tmp.path(), &requested).unwrap(),
            SessionDispositionObservation::Committed
        );
        assert_eq!(
            std::fs::read(store.trim_breadcrumb_path(key)).unwrap(),
            b"1"
        );
        SessionStore::acknowledge_original_session_disposition(tmp.path(), &requested).unwrap();
        assert!(!store.session_path(key).exists());
        assert!(!store.trim_breadcrumb_path(key).exists());
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_disposition_malformed_marker_and_replaced_instance_fail_closed() {
        use crate::session_sqlite::SqliteSessionBackend;
        let original_dir = TempDir::new().unwrap();
        let original = SessionStore::new(original_dir.path()).unwrap();
        let key = "key";
        let requested = witness(&original, key);
        original.append(key, &ChatMessage::user("old")).unwrap();
        // Use the private original file but corrupt the reserved format.
        let damage = b"\0ZC-SESSION-DISPOSITION";
        let path = original.session_path(key);
        std::fs::write(&path, damage).unwrap();
        assert!(original.load(key).is_empty());
        assert!(!original.session_exists(key));
        assert!(original.try_load(key).is_err());
        assert!(original.append(key, &ChatMessage::user("no")).is_err());
        assert!(original.delete_session(key).is_err());
        let sqlite = SqliteSessionBackend::new(original_dir.path()).unwrap();
        assert!(sqlite.migrate_from_jsonl(original_dir.path()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), damage);

        let replacement_dir = TempDir::new().unwrap();
        assert_eq!(
            SessionStore::observe_original_session_disposition(replacement_dir.path(), &requested)
                .unwrap(),
            SessionDispositionObservation::Indeterminate
        );
        assert!(!replacement_dir.path().join("sessions").exists());
        let replacement = SessionStore::new(replacement_dir.path()).unwrap();
        assert_ne!(
            replacement.session_disposition_identity().unwrap(),
            requested.identity
        );
        replacement
            .append(key, &ChatMessage::user("replacement"))
            .unwrap();
        assert_eq!(
            replacement.observe_session_disposition(&requested).unwrap(),
            SessionDispositionObservation::Indeterminate
        );
        assert!(
            replacement
                .acknowledge_session_disposition(&requested)
                .is_err()
        );
        assert_eq!(replacement.try_load(key).unwrap()[0].content, "replacement");
    }

    #[cfg(unix)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }

    #[cfg(windows)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(original, link)
    }

    #[derive(Debug, PartialEq)]
    struct SessionEntrySnapshot {
        entry_kind: &'static str,
        link_target: Option<PathBuf>,
        entry_contents: Option<Vec<u8>>,
        tracked_target_contents: Option<Option<Vec<u8>>>,
    }

    fn snapshot_session_entry(path: &Path, tracked_target: Option<&Path>) -> SessionEntrySnapshot {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let file_type = metadata.file_type();
        SessionEntrySnapshot {
            entry_kind: if file_type.is_symlink() {
                "symlink"
            } else if file_type.is_dir() {
                "directory"
            } else if file_type.is_file() {
                "file"
            } else {
                "other"
            },
            link_target: file_type
                .is_symlink()
                .then(|| std::fs::read_link(path).unwrap()),
            entry_contents: file_type.is_file().then(|| std::fs::read(path).unwrap()),
            tracked_target_contents: tracked_target.map(|target| std::fs::read(target).ok()),
        }
    }

    #[test]
    fn round_trip_append_and_load() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("telegram_user123", &ChatMessage::user("hello"))
            .unwrap();
        store
            .append("telegram_user123", &ChatMessage::assistant("hi there"))
            .unwrap();

        let messages = store.load("telegram_user123");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "hi there");
    }

    const PARTIAL_FRAMING_ROW: &str = r#"{"role":"assistant","content":"cut"#;

    fn framing_messages() -> ([ChatMessage; 3], [String; 3]) {
        let messages = [
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ];
        let rows = messages
            .each_ref()
            .map(|message| serde_json::to_string(message).unwrap());
        (messages, rows)
    }

    fn assert_framing_history(actual: &[ChatMessage], expected: &[ChatMessage]) {
        fn pairs(messages: &[ChatMessage]) -> Vec<(&str, &str)> {
            messages
                .iter()
                .map(|message| (message.role.as_str(), message.content.as_str()))
                .collect()
        }
        assert_eq!(pairs(actual), pairs(expected));
    }

    // A complete final row without LF needs one separator, even with repeated appends.
    #[test]
    fn append_preserves_complete_jsonl_final_row_without_lf() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "append-framing";
        let path = store.session_path(key);
        let ([first, second, third], [j1, j2, j3]) = framing_messages();
        let initial = format!("{j1}\n{j2}");
        std::fs::write(&path, &initial).unwrap();
        assert_framing_history(
            &store.try_load(key).unwrap(),
            &[first.clone(), second.clone()],
        );

        store.append(key, &third).unwrap();
        store.append(key, &third).unwrap();
        drop(store);
        let reopened = SessionStore::new(tmp.path()).unwrap();
        let strict = reopened.try_load(key);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{initial}\n{j3}\n{j3}\n"),
            "strict reopen: {strict:?}",
        );
        let expected = [first, second, third.clone(), third];
        assert_framing_history(&strict.unwrap(), &expected);
        assert_framing_history(&reopened.load(key), &expected);
    }

    // Preserve LF, trailing blank lines and CRLF without inserting another separator.
    #[test]
    fn append_preserves_lf_terminated_jsonl_history() {
        for ending in ["\n", "\n\n", "\r\n"] {
            let tmp = TempDir::new().unwrap();
            let store = SessionStore::new(tmp.path()).unwrap();
            let key = "append-terminated";
            let path = store.session_path(key);
            let (messages, [j1, j2, j3]) = framing_messages();
            let initial = format!("{j1}\n{j2}{ending}");
            std::fs::write(&path, &initial).unwrap();

            store.append(key, &messages[2]).unwrap();
            drop(store);
            let reopened = SessionStore::new(tmp.path()).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("{initial}{j3}\n")
            );
            assert_framing_history(&reopened.try_load(key).unwrap(), &messages);
        }
    }

    // Neither an existing empty file nor a newly created file needs a leading LF.
    #[test]
    fn append_to_empty_or_missing_session_adds_no_separator() {
        for existing in [true, false] {
            let tmp = TempDir::new().unwrap();
            let store = SessionStore::new(tmp.path()).unwrap();
            let key = "append-empty-or-missing";
            let path = store.session_path(key);
            let (messages, [j1, j2, _]) = framing_messages();
            if existing {
                std::fs::write(&path, b"").unwrap();
            }
            assert_eq!(path.exists(), existing);

            store.append(key, &messages[0]).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{j1}\n"));
            store.append(key, &messages[1]).unwrap();
            drop(store);
            let reopened = SessionStore::new(tmp.path()).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("{j1}\n{j2}\n")
            );
            assert_framing_history(&reopened.try_load(key).unwrap(), &messages[..2]);
        }
    }

    fn assert_append_leaves_damaged_tail(tail: &str, separator: &str) {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "append-damaged-tail";
        let path = store.session_path(key);
        let (messages, [j1, j2, _]) = framing_messages();
        let initial = format!("{j1}\n{tail}");
        std::fs::write(&path, &initial).unwrap();

        store.append(key, &messages[1]).unwrap();
        drop(store);
        let reopened = SessionStore::new(tmp.path()).unwrap();
        let best_effort = reopened.load(key);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{initial}{separator}{j2}\n"),
            "best-effort reopen: {best_effort:?}",
        );
        assert_framing_history(&best_effort, &messages[..2]);
        assert_eq!(
            reopened.try_load(key).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    // A partial row stays damaged, but later good history must remain readable.
    #[test]
    fn append_after_partial_final_record_keeps_new_record_distinct() {
        assert_append_leaves_damaged_tail(PARTIAL_FRAMING_ROW, "\n");
    }

    // An already terminated malformed row does not need repair or another separator.
    #[test]
    fn append_after_terminated_malformed_record_adds_no_separator() {
        assert_append_leaves_damaged_tail("not json\n", "");
    }

    // Guarded append counts valid history, commits once, and uses the same framing.
    #[test]
    fn append_authorized_frames_unterminated_record() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "append-authorized-valid";
        let path = store.session_path(key);
        let (messages, [j1, j2, j3]) = framing_messages();
        std::fs::write(&path, format!("{j1}\n{j2}")).unwrap();
        let commits = std::cell::Cell::new(0);
        let count = store
            .append_authorized(
                key,
                &messages[2],
                &|owner| {
                    assert_eq!(owner, None);
                    Ok(Box::new(()))
                },
                &mut || commits.set(commits.get() + 1),
            )
            .unwrap();
        assert_eq!((count, commits.get()), (3, 1));
        drop(store);
        let reopened = SessionStore::new(tmp.path()).unwrap();
        let strict = reopened.try_load(key);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{j1}\n{j2}\n{j3}\n"),
            "strict reopen: {strict:?}",
        );
        assert_framing_history(&strict.unwrap(), &messages);
    }

    // Guarded append must still refuse damaged history without publishing any effect.
    #[test]
    fn append_authorized_refuses_malformed_tail_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "append-authorized-damaged";
        let path = store.session_path(key);
        let (messages, [j1, _, _]) = framing_messages();
        let initial = format!("{j1}\n{PARTIAL_FRAMING_ROW}");
        std::fs::write(&path, &initial).unwrap();
        let committed = std::cell::Cell::new(false);
        let error = store
            .append_authorized(
                key,
                &messages[1],
                &|owner| {
                    assert_eq!(owner, None);
                    Ok(Box::new(()))
                },
                &mut || committed.set(true),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!committed.get());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);
    }

    #[test]
    fn damaged_jsonl_has_only_a_best_effort_history() {
        use std::io::Write;

        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "history";
        store.append(key, &ChatMessage::user("valid")).unwrap();
        let clean = store.try_load(key).unwrap();
        assert_eq!(clean.len(), 1);
        assert_eq!(store.load(key)[0].content, clean[0].content);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(store.session_path(key))
            .unwrap();
        writeln!(file, "invalid JSONL").unwrap();
        assert_eq!(
            store.try_load(key).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let best_effort = store.load(key);
        assert_eq!(best_effort.len(), 1);
        assert_eq!(best_effort[0].content, "valid");
    }

    #[test]
    fn trim_breadcrumb_round_trips_and_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), None);

        store.set_trim_breadcrumb("chan_user", true).unwrap();
        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), Some(true));

        // A fresh store instance simulates a process restart: the flag must
        // be a durable fact, not held only in an in-process cache.
        let reopened = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(
            reopened.get_trim_breadcrumb("chan_user").unwrap(),
            Some(true)
        );

        store.set_trim_breadcrumb("chan_user", false).unwrap();
        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), Some(false));
    }

    #[test]
    fn get_trim_breadcrumb_rejects_an_empty_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        std::fs::write(store.trim_breadcrumb_path("chan_user"), b"").unwrap();

        let err = store.get_trim_breadcrumb("chan_user").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn get_trim_breadcrumb_rejects_an_unrecognized_byte() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        std::fs::write(store.trim_breadcrumb_path("chan_user"), b"2").unwrap();

        let err = store.get_trim_breadcrumb("chan_user").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn get_trim_breadcrumb_rejects_trailing_garbage() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        std::fs::write(store.trim_breadcrumb_path("chan_user"), b"10").unwrap();

        let err = store.get_trim_breadcrumb("chan_user").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn deleting_a_session_removes_its_breadcrumb_flag() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store.append("chan_user", &ChatMessage::user("hi")).unwrap();
        store.set_trim_breadcrumb("chan_user", true).unwrap();

        store.delete_session("chan_user").unwrap();

        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), None);
    }

    #[test]
    fn load_nonexistent_session_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let messages = store.load("nonexistent");
        assert!(messages.is_empty());
    }

    #[test]
    fn key_sanitization() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("slack/thread:123/user", &ChatMessage::user("test"))
            .unwrap();

        let messages = store.load("slack/thread:123/user");
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn sanitize_session_key_is_idempotent() {
        let raw = "slack_C123_1.2_user one";
        let once = sanitize_session_key(raw);
        let twice = sanitize_session_key(&once);
        assert_eq!(once, "slack_C123_1_2_user_one");
        assert_eq!(once, twice);
    }

    #[test]
    fn restart_simulation_matches_when_caller_pre_sanitizes() {
        let tmp = TempDir::new().unwrap();
        let runtime_key = sanitize_session_key("slack_C123_1.2_user one");

        {
            let store = SessionStore::new(tmp.path()).unwrap();
            store
                .append(&runtime_key, &ChatMessage::user("first"))
                .unwrap();
            store
                .append(&runtime_key, &ChatMessage::assistant("ack"))
                .unwrap();
        }

        let store = SessionStore::new(tmp.path()).unwrap();
        let listed = store.list_sessions();
        assert_eq!(listed, vec![runtime_key.clone()]);

        let msgs = store.load(&listed[0]);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "ack");
    }

    #[test]
    fn list_sessions_returns_keys() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("telegram_alice", &ChatMessage::user("hi"))
            .unwrap();
        store
            .append("discord_bob", &ChatMessage::user("hey"))
            .unwrap();

        let mut sessions = store.list_sessions();
        sessions.sort();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"discord_bob".to_string()));
        assert!(sessions.contains(&"telegram_alice".to_string()));
    }

    #[test]
    fn session_operations_accept_only_regular_jsonl_files() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        let sessions_dir = tmp.path().join("sessions");

        backend
            .append("valid", &ChatMessage::user("persisted message"))
            .unwrap();
        std::fs::create_dir(sessions_dir.join("directory.jsonl")).unwrap();
        std::fs::write(sessions_dir.join("notes.txt"), "not a session").unwrap();

        let mut invalid_entries = vec![("directory", None)];

        #[cfg(any(unix, windows))]
        {
            let linked = sessions_dir.join("linked.jsonl");
            match symlink_file(&store.session_path("valid"), &linked) {
                Ok(()) => {
                    symlink_file(
                        &sessions_dir.join("missing.jsonl"),
                        &sessions_dir.join("dangling.jsonl"),
                    )
                    .unwrap();
                    invalid_entries.extend([
                        ("linked", Some(store.session_path("valid"))),
                        ("dangling", Some(sessions_dir.join("missing.jsonl"))),
                    ]);
                }
                #[cfg(windows)]
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
                Err(error) => panic!("failed to create session symlink fixture: {error}"),
            }
        }

        assert_eq!(store.list_sessions(), vec!["valid".to_string()]);
        assert!(backend.session_exists("valid"));
        assert_eq!(backend.load("valid").len(), 1);
        assert!(store.session_mtime("valid").is_some());

        for (key, _) in &invalid_entries {
            assert!(!backend.session_exists(key), "{key} must not exist");
            assert!(backend.load(key).is_empty(), "{key} must not load");
            assert!(
                store.session_mtime(key).is_none(),
                "{key} must not expose an mtime"
            );
            assert_eq!(
                backend.clear_messages(key).unwrap(),
                0,
                "{key} must not be cleared"
            );
            assert!(
                !backend.delete_session(key).unwrap(),
                "{key} must not be deleted"
            );
            assert!(
                std::fs::symlink_metadata(store.session_path(key)).is_ok(),
                "{key} filesystem entry must remain untouched"
            );
        }

        let rejected_message = ChatMessage::user("must not be persisted");
        let mut mutation_failures = Vec::new();
        for (key, tracked_target) in &invalid_entries {
            let path = store.session_path(key);
            let before = snapshot_session_entry(&path, tracked_target.as_deref());
            let append_result = backend.append(key, &rejected_message);
            let compact_result = backend.compact(key);
            let after = snapshot_session_entry(&path, tracked_target.as_deref());

            if append_result.is_ok() || compact_result.is_ok() || after != before {
                mutation_failures.push(format!(
                    "{key}: append={append_result:?}, compact={compact_result:?}, before={before:?}, after={after:?}"
                ));
            }
        }

        backend
            .append("new-session", &ChatMessage::user("new session works"))
            .unwrap();
        assert_eq!(backend.load("new-session").len(), 1);

        assert!(
            mutation_failures.is_empty(),
            "append/compact must reject invalid entries without modifying entries or targets:\n{}",
            mutation_failures.join("\n")
        );

        assert_eq!(backend.clear_messages("valid").unwrap(), 1);
        assert!(backend.session_exists("valid"));
        assert!(backend.delete_session("valid").unwrap());
        assert!(!backend.session_exists("valid"));
        assert!(sessions_dir.join("notes.txt").is_file());
    }

    #[test]
    fn append_is_truly_append_only() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "test_session";

        store.append(key, &ChatMessage::user("msg1")).unwrap();
        store.append(key, &ChatMessage::user("msg2")).unwrap();

        // Read raw file to verify append-only format
        let path = store.session_path(key);
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn remove_last_drops_final_message() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("rm_test", &ChatMessage::user("first"))
            .unwrap();
        store
            .append("rm_test", &ChatMessage::user("second"))
            .unwrap();

        assert!(store.remove_last("rm_test").unwrap());
        let messages = store.load("rm_test");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "first");
    }

    #[test]
    fn remove_last_empty_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        assert!(!store.remove_last("nonexistent").unwrap());
    }

    #[test]
    fn update_last_via_trait_replaces_final_message() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        let key = "update_test";

        backend.append(key, &ChatMessage::user("first")).unwrap();
        backend.append(key, &ChatMessage::assistant("old")).unwrap();

        assert!(
            backend
                .update_last(key, &ChatMessage::assistant("new"))
                .unwrap()
        );

        let messages = backend.load(key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "new");
    }

    #[test]
    fn failed_rewrite_preserves_original_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "rewrite_failure";

        store.append(key, &ChatMessage::user("first")).unwrap();
        store
            .append(key, &ChatMessage::assistant("second"))
            .unwrap();
        let path = store.session_path(key);
        let original = std::fs::read(&path).unwrap();

        let mut temp_path = None;
        let result = store.rewrite_with(key, &[ChatMessage::user("replacement")], |temp, _path| {
            temp_path = Some(temp.path().to_path_buf());
            Err(std::io::Error::other("injected persist failure"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!temp_path.unwrap().exists());
    }

    /// Readers take no lock, so one can hold a session file open while a
    /// writer replaces it. That must not stop the replace, and the reader
    /// keeps the transcript it opened. Windows refuses a plain replace of an
    /// open file, so this pins the replace itself rather than leaving it to a
    /// race.
    #[test]
    fn a_rewrite_replaces_a_session_file_a_reader_holds_open() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        let key = "held_open";
        backend.append(key, &ChatMessage::user("first")).unwrap();
        backend.append(key, &ChatMessage::assistant("old")).unwrap();
        let mut held = std::fs::File::open(store.session_path(key)).unwrap();

        backend
            .replace_conversation_state(
                key,
                &[ChatMessage::user("first"), ChatMessage::assistant("new")],
                false,
            )
            .expect("an open reader must not stop the rewrite");

        let held_transcript = std::io::read_to_string(&mut held).unwrap();
        assert!(held_transcript.contains("old"), "{held_transcript}");
        let messages = backend.load(key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, "new");
    }

    #[test]
    fn concurrent_append_waits_for_update_last_commit() {
        let tmp = TempDir::new().unwrap();
        let update_store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let append_store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let key = "concurrent_update";
        update_store
            .append(key, &ChatMessage::user("first"))
            .unwrap();
        update_store
            .append(key, &ChatMessage::assistant("old"))
            .unwrap();

        let (staged_tx, staged_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let update_worker = Arc::clone(&update_store);
        let updater = std::thread::spawn(move || {
            update_worker.update_last_with(key, &ChatMessage::assistant("new"), |temp, path| {
                staged_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                publish_staged_rewrite(temp, path)
            })
        });

        staged_rx.recv().unwrap();
        let (append_started_tx, append_started_rx) = mpsc::channel();
        let (append_done_tx, append_done_rx) = mpsc::channel();
        let append_store = Arc::clone(&append_store);
        let appender = std::thread::spawn(move || {
            append_started_tx.send(()).unwrap();
            let result = append_store.append(key, &ChatMessage::user("concurrent"));
            append_done_tx.send(()).unwrap();
            result
        });

        append_started_rx.recv().unwrap();
        assert!(
            append_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );

        release_tx.send(()).unwrap();
        assert!(updater.join().unwrap().unwrap());
        appender.join().unwrap().unwrap();

        let messages = update_store.load(key);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "new");
        assert_eq!(messages[2].content, "concurrent");
    }

    #[test]
    fn compact_removes_corrupt_lines() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "compact_test";

        let path = store.session_path(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"role":"user","content":"ok"}}"#).unwrap();
        writeln!(file, "corrupt line").unwrap();
        writeln!(file, r#"{{"role":"assistant","content":"hi"}}"#).unwrap();
        drop(file);

        store.compact(key).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.trim().lines().count(), 2);
    }

    #[test]
    fn session_backend_trait_works_via_dyn() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("trait_test", &ChatMessage::user("hello"))
            .unwrap();
        let msgs = backend.load("trait_test");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn handles_corrupt_lines_gracefully() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "corrupt_test";

        // Write valid message + corrupt line + valid message
        let path = store.session_path(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"role":"user","content":"hello"}}"#).unwrap();
        writeln!(file, "this is not valid json").unwrap();
        writeln!(file, r#"{{"role":"assistant","content":"world"}}"#).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].content, "world");
    }

    #[test]
    fn clear_messages_truncates_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "clear_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.append(key, &ChatMessage::assistant("world")).unwrap();

        let cleared = store.clear_messages(key).unwrap();
        assert_eq!(cleared, 2);
        assert!(store.load(key).is_empty());
        // File still exists — session key remains in list_sessions
        assert!(store.session_path(key).exists());
    }

    #[test]
    fn clear_messages_empty_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(store.clear_messages("nonexistent").unwrap(), 0);
    }

    #[test]
    fn clear_messages_does_not_affect_other_sessions() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("alice", &ChatMessage::user("alice msg"))
            .unwrap();
        store.append("bob", &ChatMessage::user("bob msg")).unwrap();

        store.clear_messages("alice").unwrap();
        assert!(store.load("alice").is_empty());
        assert_eq!(store.load("bob").len(), 1);
    }

    #[test]
    fn clear_messages_then_append_works() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "reuse_test";

        store.append(key, &ChatMessage::user("old")).unwrap();
        store.clear_messages(key).unwrap();
        store.append(key, &ChatMessage::user("new")).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "new");
    }

    #[test]
    fn clear_messages_removes_trim_breadcrumb_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "crumb_reset_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), Some(true));

        store.clear_messages(key).unwrap();

        // A reset session has no synthetic marker; the recorded flag must
        // not survive as a stale `true` for the next first message.
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);
    }

    #[test]
    fn clear_messages_removes_stale_sidecar_even_when_already_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "already_empty_crumb_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        store.clear_messages(key).unwrap();
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);

        // Clearing an already-empty session must not leave a stale flag
        // behind either.
        store.set_trim_breadcrumb(key, true).unwrap();
        assert_eq!(store.clear_messages(key).unwrap(), 0);
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);
    }

    #[test]
    fn clear_messages_removes_sidecar_only_breadcrumb_with_no_transcript_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "sidecar_only_crumb_test";

        // No transcript file exists at all for this key (e.g. after a prior
        // cleanup removed the transcript but not its sidecar), yet the
        // sidecar is independently creatable.
        store.set_trim_breadcrumb(key, true).unwrap();
        assert!(!is_regular_jsonl_session_file(&store.session_path(key)));
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), Some(true));

        assert_eq!(store.clear_messages(key).unwrap(), 0);
        assert_eq!(
            store.get_trim_breadcrumb(key).unwrap(),
            None,
            "a sidecar-only breadcrumb must not survive clear_messages just because \
             there was no transcript file to early-return past"
        );

        // A session that later receives its first message must not inherit
        // the stale flag and misclassify that message as post-trim.
        store
            .append(key, &ChatMessage::user("first message"))
            .unwrap();
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);
    }

    #[test]
    fn delete_session_removes_jsonl_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "delete_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        assert_eq!(store.load(key).len(), 1);

        let deleted = store.delete_session(key).unwrap();
        assert!(deleted);
        assert!(store.load(key).is_empty());
        assert!(!store.session_path(key).exists());
    }

    #[test]
    fn delete_session_nonexistent_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let deleted = store.delete_session("nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn delete_session_via_trait() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("trait_delete", &ChatMessage::user("hello"))
            .unwrap();
        assert_eq!(backend.load("trait_delete").len(), 1);

        let deleted = backend.delete_session("trait_delete").unwrap();
        assert!(deleted);
        assert!(backend.load("trait_delete").is_empty());
    }

    // ── session_exists─────────────────────────────────────
    #[test]
    fn session_exists_tracks_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        assert!(!backend.session_exists("ghost"));

        backend
            .append("ghost", &ChatMessage::user("first"))
            .unwrap();
        assert!(backend.session_exists("ghost"));

        backend.delete_session("ghost").unwrap();
        assert!(!backend.session_exists("ghost"));
    }

    #[test]
    fn replace_conversation_state_if_exists_is_a_no_op_once_deletion_wins() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend.append("gone", &ChatMessage::user("turn")).unwrap();
        assert!(backend.delete_session("gone").unwrap());

        // Deterministic delete-versus-completion ordering: the deleter (which
        // holds the same mutation guard) commits first, then the post-turn
        // write arrives. It must become a no-op instead of recreating the
        // transcript and sidecar files the delete just removed.
        let written = backend
            .replace_conversation_state_if_exists("gone", &[ChatMessage::user("late turn")], false)
            .unwrap();
        assert!(
            !written,
            "post-turn write must be a no-op once deletion wins"
        );
        assert!(!backend.session_exists("gone"));
        assert!(backend.load("gone").is_empty());

        // The live path still writes transcript and flag together.
        backend.append("live", &ChatMessage::user("turn")).unwrap();
        let written = backend
            .replace_conversation_state_if_exists("live", &[ChatMessage::user("new")], true)
            .unwrap();
        assert!(written);
        assert_eq!(backend.load("live").len(), 1);
        assert_eq!(
            backend.get_session_trim_breadcrumb("live").unwrap(),
            Some(true)
        );
    }

    // ── get_session_metadata (trait default) tests ──────────────────

    #[test]
    fn get_session_metadata_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        assert!(backend.get_session_metadata("nonexistent").is_none());
    }

    #[test]
    fn get_session_metadata_returns_correct_count() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("test_session", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .append("test_session", &ChatMessage::assistant("hi"))
            .unwrap();

        let meta = backend.get_session_metadata("test_session").unwrap();
        assert_eq!(meta.key, "test_session");
        assert_eq!(meta.message_count, 2);
        assert!(meta.name.is_none());
    }

    #[test]
    fn replace_conversation_state_rolls_back_transcript_when_breadcrumb_write_fails() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("s1", &ChatMessage::user("pre-replace turn"))
            .unwrap();
        backend.set_session_trim_breadcrumb("s1", false).unwrap();

        // Force the breadcrumb half of the write to fail by occupying its
        // path with a directory: `fs::write` errors instead of replacing it.
        let breadcrumb_path = store.trim_breadcrumb_path("s1");
        std::fs::remove_file(&breadcrumb_path).unwrap();
        std::fs::create_dir(&breadcrumb_path).unwrap();

        let result = backend.replace_conversation_state(
            "s1",
            &[ChatMessage::user(
                "replacement turn that must not land alone",
            )],
            true,
        );
        assert!(
            result.is_err(),
            "the poisoned breadcrumb path must fail the call"
        );

        // The transcript must have rolled back to its pre-replace content,
        // not the replacement that could never be paired with a committed
        // flag.
        let messages = backend.load("s1");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "pre-replace turn");
    }
}
