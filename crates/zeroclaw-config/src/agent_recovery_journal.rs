//! Durable recovery journal for agent lifecycle operations.
//!
//! Renaming an agent commits the config change first; the stores that keep
//! state under the agent's alias (workspace, memory, cron, ACP, sessions)
//! converge afterwards. A failure between the two would strand that state
//! under the old alias, and an agent later created under the old alias would
//! inherit it. The journal records the operation before the config commit, so
//! the runtime can finish it and the agent-creation choke points can refuse
//! the old alias until it is finished.
//!
//! The journal is one file in the instance data directory holding every
//! unfinished operation. Writers serialize on an advisory lock over a sidecar
//! file and publish each revision through a synced temp file and an atomic
//! rename. Readers never lock: they see one whole revision or the other.
//!
//! Only an effective record retires an alias, see
//! [`RecoveryRecord::is_effective`]. Every read failure fails closed: a
//! journal that cannot be read may hold an effective record, so the guards
//! refuse rather than guess.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::schema::Config;

/// File name of the journal inside the data directory.
pub const JOURNAL_FILE_NAME: &str = "agent-lifecycle-recovery.json";
/// Sidecar file that writers lock. It is created on first use and never
/// deleted, so no writer can lock a file another writer is about to unlink.
pub const JOURNAL_LOCK_FILE_NAME: &str = "agent-lifecycle-recovery.json.lock";
/// Prefix of the temp files a write stages before renaming over the journal.
pub const JOURNAL_TEMP_PREFIX: &str = ".agent-lifecycle-recovery.json.tmp-";
/// The only journal format this build reads or writes.
pub const SCHEMA_VERSION: u32 = 1;

/// How often [`AgentRecoveryJournal::lock`] retries a lock another writer holds.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The lifecycle operation a record tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOperation {
    /// The agent `from` is being renamed to `to`.
    Rename,
}

/// How far an operation had got when it was last journaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    /// Armed before the config commit. Whether the commit landed is read off
    /// the live config.
    Prepared,
    /// The config commit landed; the followers have not all converged.
    Committed,
}

/// One unfinished lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub operation: RecoveryOperation,
    /// The alias the operation retires.
    pub from: String,
    /// The alias the operation's state converges to.
    pub to: String,
    pub phase: RecoveryPhase,
    /// Absolute default per-alias workspace of `from` captured before the
    /// commit; None when `from` set a `workspace.path`, even one naming its
    /// default location, which does not follow the alias and must not be
    /// moved. The runtime moves it only while it is still `from`'s default
    /// location under the live config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_workspace: Option<PathBuf>,
    /// RFC 3339 timestamp.
    pub armed_at: String,
}

impl RecoveryRecord {
    /// Whether the record retires `from`. A `Committed` record always does;
    /// a `Prepared` one does once the live config no longer has `from`.
    /// Whether `to` is configured does not matter: a `from` that is gone may
    /// have left state behind whatever happened to `to` since, so the record
    /// fails closed and keeps `from` retired.
    ///
    /// A `Prepared` record that is not effective is void: `from` is still
    /// configured, so its commit never landed, and it retires nothing and has
    /// nothing to converge.
    #[must_use]
    pub fn is_effective(&self, config: &Config) -> bool {
        match self.phase {
            RecoveryPhase::Committed => true,
            RecoveryPhase::Prepared => !config.agents.contains_key(&self.from),
        }
    }
}

/// Why a journal operation failed. Each `path` is the absolute path of the
/// file involved, for logs; [`Display`](std::fmt::Display) names the file
/// only by its file name.
#[derive(Debug)]
pub enum JournalError {
    /// The journal exists but could not be read or parsed. It may hold an
    /// effective record, so callers fail closed.
    Unreadable { path: PathBuf, detail: String },
    /// The journal declares a format this build does not read.
    UnsupportedSchema { path: PathBuf, version: u32 },
    /// Another writer held the journal lock for the whole wait.
    Busy { path: PathBuf },
    /// The journal, its lock, or its directory could not be written.
    Write { path: PathBuf, detail: String },
}

impl std::fmt::Display for JournalError {
    // The text reaches gateway and daemon RPC error bodies, some of them
    // rendered before the caller is authorized, so it names the journal file
    // and never where the install keeps it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, detail } => {
                write!(f, "{} is unreadable: {detail}", file_label(path))
            }
            Self::UnsupportedSchema { path, version } => write!(
                f,
                "{} uses schema_version {version}; this build reads {SCHEMA_VERSION}",
                file_label(path)
            ),
            Self::Busy { path } => write!(
                f,
                "{} is locked by another agent lifecycle operation",
                file_label(path)
            ),
            Self::Write { path, detail } => {
                write!(f, "cannot write {}: {detail}", file_label(path))
            }
        }
    }
}

/// The journal file `path` names, by file name alone: the lock file, or else
/// the journal itself, which also stands for its directory and temp files.
fn file_label(path: &Path) -> &'static str {
    if path
        .file_name()
        .is_some_and(|name| name == JOURNAL_LOCK_FILE_NAME)
    {
        JOURNAL_LOCK_FILE_NAME
    } else {
        JOURNAL_FILE_NAME
    }
}

impl std::error::Error for JournalError {}

/// Proof that the caller holds a journal's writer lock. The lock lives on the
/// open file handle, so the kernel also releases it when a holder dies.
#[derive(Debug)]
pub struct JournalGuard {
    file: File,
    lock_path: PathBuf,
}

impl Drop for JournalGuard {
    fn drop(&mut self) {
        // Best effort: closing the handle right after releases it too.
        let _ = self.file.unlock();
    }
}

/// On-disk shape of the journal.
#[derive(Serialize, Deserialize)]
struct JournalFile {
    schema_version: u32,
    records: Vec<RecoveryRecord>,
}

/// Reads only the version, so a journal in a newer format reports
/// [`JournalError::UnsupportedSchema`] instead of failing on its records.
#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

/// The journal of one data directory.
#[derive(Debug, Clone)]
pub struct AgentRecoveryJournal {
    path: PathBuf,
    lock_path: PathBuf,
}

impl AgentRecoveryJournal {
    #[must_use]
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(JOURNAL_FILE_NAME),
            lock_path: data_dir.join(JOURNAL_LOCK_FILE_NAME),
        }
    }

    #[must_use]
    pub fn for_config(config: &Config) -> Self {
        Self::for_data_dir(&config.data_dir)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every record in the journal. A missing journal has none. Any other
    /// read or parse failure is [`JournalError::Unreadable`], and a journal
    /// declaring another format is [`JournalError::UnsupportedSchema`].
    pub fn load(&self) -> Result<Vec<RecoveryRecord>, JournalError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.unreadable(&e)),
        };
        let probe: SchemaProbe = serde_json::from_slice(&bytes).map_err(|e| self.unreadable(&e))?;
        if probe.schema_version != SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema {
                path: self.path.clone(),
                version: probe.schema_version,
            });
        }
        let file: JournalFile = serde_json::from_slice(&bytes).map_err(|e| self.unreadable(&e))?;
        Ok(file.records)
    }

    /// Take the exclusive writer lock, retrying every 20 ms until `wait`
    /// elapses and then giving up with [`JournalError::Busy`]. The retries
    /// sleep the calling thread.
    ///
    /// The lock is advisory and belongs to the open file description, so a
    /// fresh handle per call serializes threads of one process as well as
    /// separate processes. It is not reentrant: a second call while the caller
    /// still holds a guard waits out `wait` and fails. A config without a data
    /// directory has no journal, so there is nothing to lock.
    pub fn lock(&self, wait: Duration) -> Result<JournalGuard, JournalError> {
        let dir = self.dir();
        if dir.as_os_str().is_empty() {
            return Err(JournalError::Write {
                path: self.lock_path.clone(),
                detail: "no data directory is configured".to_string(),
            });
        }
        std::fs::create_dir_all(dir).map_err(|e| self.lock_error(&e))?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&self.lock_path)
            .map_err(|e| self.lock_error(&e))?;

        // `None` only for a wait too long to represent: never give up.
        let deadline = Instant::now().checked_add(wait);
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(JournalGuard {
                        file,
                        lock_path: self.lock_path.clone(),
                    });
                }
                Err(TryLockError::WouldBlock) => {
                    let now = Instant::now();
                    let pause = match deadline {
                        Some(deadline) if now >= deadline => {
                            return Err(JournalError::Busy {
                                path: self.lock_path.clone(),
                            });
                        }
                        Some(deadline) => LOCK_POLL_INTERVAL.min(deadline - now),
                        None => LOCK_POLL_INTERVAL,
                    };
                    std::thread::sleep(pause);
                }
                Err(TryLockError::Error(e)) => return Err(self.lock_error(&e)),
            }
        }
    }

    /// Record `record`, replacing the record with the same operation and
    /// `from`, or appending it when there is none.
    pub fn upsert(&self, guard: &JournalGuard, record: RecoveryRecord) -> Result<(), JournalError> {
        self.check_guard(guard)?;
        let mut records = self.load()?;
        let same = |r: &RecoveryRecord| r.operation == record.operation && r.from == record.from;
        let position = records.iter().position(same);
        records.retain(|r| !same(r));
        match position {
            // Every record before the first match survives the retain, so
            // `index` stays in bounds; the clamp only keeps `insert` panic-free.
            Some(index) => records.insert(index.min(records.len()), record),
            None => records.push(record),
        }
        self.store(records)
    }

    /// Drop the record for `operation` on `from`. Returns whether one was
    /// there. The journal file is deleted once no records remain.
    pub fn remove(
        &self,
        guard: &JournalGuard,
        operation: RecoveryOperation,
        from: &str,
    ) -> Result<bool, JournalError> {
        self.check_guard(guard)?;
        let mut records = self.load()?;
        let before = records.len();
        records.retain(|r| !(r.operation == operation && r.from == from));
        if records.len() == before {
            return Ok(false);
        }
        self.store(records)?;
        Ok(true)
    }

    /// Drop every void record, a `Prepared` one whose commit never landed in
    /// `config`, and return how many were dropped.
    ///
    /// A record is void only once its operation can no longer commit, so run
    /// this while no operation sits between its prepare and its commit.
    /// Holding the guard across that window, as the writer must, guarantees it.
    pub fn collect_void(
        &self,
        guard: &JournalGuard,
        config: &Config,
    ) -> Result<usize, JournalError> {
        self.check_guard(guard)?;
        let mut records = self.load()?;
        let before = records.len();
        records.retain(|r| r.is_effective(config));
        let dropped = before - records.len();
        if dropped > 0 {
            self.store(records)?;
        }
        Ok(dropped)
    }

    fn dir(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new(""))
    }

    /// Refuse a guard taken on another journal: it proves nothing about this
    /// one's lock.
    fn check_guard(&self, guard: &JournalGuard) -> Result<(), JournalError> {
        if guard.lock_path == self.lock_path {
            return Ok(());
        }
        Err(JournalError::Write {
            path: self.path.clone(),
            detail: "the held lock belongs to another journal".to_string(),
        })
    }

    /// Publish `records` as the whole journal, or delete the journal when
    /// none remain.
    fn store(&self, records: Vec<RecoveryRecord>) -> Result<(), JournalError> {
        if records.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => sync_dir(self.dir()).map_err(|e| self.write_error(&e)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(self.write_error(&e)),
            };
        }
        let file = JournalFile {
            schema_version: SCHEMA_VERSION,
            records,
        };
        let mut bytes = serde_json::to_vec_pretty(&file).map_err(|e| self.write_error(&e))?;
        bytes.push(b'\n');
        write_atomic(self.dir(), &self.path, &bytes).map_err(|e| self.write_error(&e))
    }

    fn unreadable(&self, error: &dyn std::fmt::Display) -> JournalError {
        JournalError::Unreadable {
            path: self.path.clone(),
            detail: error.to_string(),
        }
    }

    fn write_error(&self, error: &dyn std::fmt::Display) -> JournalError {
        JournalError::Write {
            path: self.path.clone(),
            detail: error.to_string(),
        }
    }

    fn lock_error(&self, error: &dyn std::fmt::Display) -> JournalError {
        JournalError::Write {
            path: self.lock_path.clone(),
            detail: error.to_string(),
        }
    }
}

/// Stage `bytes` in a fresh owner-only temp file beside `path`, sync it,
/// rename it over `path`, then sync the directory so the rename survives a
/// crash. A failed attempt removes its temp file.
fn write_atomic(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!("{JOURNAL_TEMP_PREFIX}{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&tmp_path)?;
    let result = publish(file, &tmp_path, path, dir, bytes);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn publish(
    mut file: File,
    tmp_path: &Path,
    path: &Path,
    dir: &Path,
    bytes: &[u8],
) -> std::io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp_path, path)?;
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()?;
    }

    #[cfg(not(unix))]
    {
        // std does not expose a portable directory-sync primitive here.
        let _ = dir;
    }

    Ok(())
}

/// True for the journal file, its lock file, and its temp files. The security
/// policy protects these names from agent file tools: an edit could hide an
/// unfinished rename from the create guards.
#[must_use]
pub fn is_journal_file_name(file_name: &str) -> bool {
    file_name == JOURNAL_FILE_NAME
        || file_name == JOURNAL_LOCK_FILE_NAME
        || file_name.starts_with(JOURNAL_TEMP_PREFIX)
}

/// The effective record that retires `alias`, the `from` of an unfinished
/// operation. Reads without the lock. A config without a data directory has
/// no journal, so nothing is retired.
pub fn retired_alias(config: &Config, alias: &str) -> Result<Option<RecoveryRecord>, JournalError> {
    find_effective(config, |record| record.from == alias)
}

/// The effective record whose state is still converging to `alias`, the `to`
/// of an unfinished operation. Reads without the lock. A config without a
/// data directory has no journal, so nothing is pending.
pub fn pending_target(
    config: &Config,
    alias: &str,
) -> Result<Option<RecoveryRecord>, JournalError> {
    find_effective(config, |record| record.to == alias)
}

fn find_effective(
    config: &Config,
    matches: impl Fn(&RecoveryRecord) -> bool,
) -> Result<Option<RecoveryRecord>, JournalError> {
    if config.data_dir.as_os_str().is_empty() {
        return Ok(None);
    }
    let records = AgentRecoveryJournal::for_config(config).load()?;
    Ok(records
        .into_iter()
        .find(|record| matches(record) && record.is_effective(config)))
}

/// Warn about each configured agent alias that an unfinished rename retired.
/// The create guards refuse such an alias, so it reached config around them
/// (a hand edit, say), and state the rename has not moved yet is still keyed
/// to it. Reads the journal once however many agents are configured, and
/// never fails: the load must come up so the operator can repair it.
pub(crate) fn warn_retired_configured_aliases(config: &Config) {
    if config.agents.is_empty() || config.data_dir.as_os_str().is_empty() {
        return;
    }
    let journal = AgentRecoveryJournal::for_config(config);
    let records = match journal.load() {
        Ok(records) => records,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error_key": "config.recovery_journal_unreadable",
                        "journal": journal.path().display().to_string(),
                        "error": error.to_string(),
                    })),
                "agent lifecycle recovery journal could not be read; agent creation is refused until it is repaired"
            );
            return;
        }
    };
    let mut aliases: Vec<&str> = config.agents.keys().map(String::as_str).collect();
    aliases.sort_unstable();
    for alias in aliases {
        let Some(record) = records
            .iter()
            .find(|record| record.from == alias && record.is_effective(config))
        else {
            continue;
        };
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "error_key": "config.retired_agent_alias_configured",
                    "alias": alias,
                    "pending_to": record.to,
                    "journal": journal.path().display().to_string(),
                })),
            "configured agent alias is retired by an unfinished agent rename; state the rename has not moved yet is still keyed to it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AliasedAgentConfig;

    fn record(from: &str, to: &str, phase: RecoveryPhase) -> RecoveryRecord {
        RecoveryRecord {
            operation: RecoveryOperation::Rename,
            from: from.to_string(),
            to: to.to_string(),
            phase,
            source_workspace: None,
            armed_at: "2026-01-01T00:00:00+00:00".to_string(),
        }
    }

    fn prepared(from: &str, to: &str) -> RecoveryRecord {
        record(from, to, RecoveryPhase::Prepared)
    }

    fn committed(from: &str, to: &str) -> RecoveryRecord {
        record(from, to, RecoveryPhase::Committed)
    }

    /// A config whose data directory is `data_dir` and whose only agents are
    /// `aliases`.
    fn config_with_agents(data_dir: &Path, aliases: &[&str]) -> Config {
        let mut config = Config {
            data_dir: data_dir.to_path_buf(),
            ..Config::default()
        };
        config.agents.clear();
        for alias in aliases {
            config
                .agents
                .insert((*alias).to_string(), AliasedAgentConfig::default());
        }
        config
    }

    fn dir_entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn load_of_a_missing_journal_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        assert_eq!(journal.path(), dir.path().join(JOURNAL_FILE_NAME));
        assert_eq!(journal.load().unwrap(), Vec::new());

        // A data directory that does not exist yet has no journal either.
        let absent = AgentRecoveryJournal::for_data_dir(&dir.path().join("absent"));
        assert_eq!(absent.load().unwrap(), Vec::new());
    }

    #[test]
    fn upsert_remove_and_collect_void_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let guard = journal.lock(Duration::ZERO).unwrap();

        journal.upsert(&guard, prepared("alpha", "beta")).unwrap();
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(journal.path()).unwrap()).unwrap();
        assert_eq!(on_disk["schema_version"], 1);
        assert_eq!(on_disk["records"][0]["operation"], "rename");
        assert_eq!(on_disk["records"][0]["phase"], "prepared");
        assert!(
            on_disk["records"][0].get("source_workspace").is_none(),
            "an absent source workspace is omitted: {on_disk}"
        );

        journal.upsert(&guard, committed("gamma", "delta")).unwrap();
        assert_eq!(
            journal.load().unwrap(),
            vec![prepared("alpha", "beta"), committed("gamma", "delta")]
        );

        // The same operation on the same alias replaces its record in place.
        let mut moved = committed("alpha", "beta");
        moved.source_workspace = Some(dir.path().join("agents").join("alpha"));
        journal.upsert(&guard, moved.clone()).unwrap();
        assert_eq!(
            journal.load().unwrap(),
            vec![moved, committed("gamma", "delta")]
        );
        journal.upsert(&guard, prepared("alpha", "beta")).unwrap();
        journal.upsert(&guard, prepared("epsilon", "zeta")).unwrap();

        // `alpha` is still configured, so its rename never committed: void.
        // `epsilon` is gone and `zeta` exists, so that rename committed.
        let config = config_with_agents(dir.path(), &["alpha", "zeta"]);
        assert_eq!(journal.collect_void(&guard, &config).unwrap(), 1);
        assert_eq!(
            journal.load().unwrap(),
            vec![committed("gamma", "delta"), prepared("epsilon", "zeta")]
        );
        assert_eq!(journal.collect_void(&guard, &config).unwrap(), 0);

        assert!(
            !journal
                .remove(&guard, RecoveryOperation::Rename, "alpha")
                .unwrap()
        );
        assert!(
            journal
                .remove(&guard, RecoveryOperation::Rename, "gamma")
                .unwrap()
        );
        assert_eq!(journal.load().unwrap(), vec![prepared("epsilon", "zeta")]);
        assert!(
            journal
                .remove(&guard, RecoveryOperation::Rename, "epsilon")
                .unwrap()
        );

        // An empty journal is deleted; no staged temp file is left behind,
        // and the lock file stays for the next writer.
        assert!(!journal.path().exists());
        assert_eq!(journal.load().unwrap(), Vec::new());
        assert_eq!(
            dir_entries(dir.path()),
            vec![JOURNAL_LOCK_FILE_NAME.to_string()]
        );
    }

    #[test]
    fn an_unsupported_schema_version_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let future = r#"{"schema_version":2,"records":[{"operation":"merge"}]}"#;
        std::fs::write(journal.path(), future).unwrap();

        assert!(matches!(
            journal.load(),
            Err(JournalError::UnsupportedSchema { version: 2, .. })
        ));
        let config = config_with_agents(dir.path(), &[]);
        assert!(retired_alias(&config, "alpha").is_err());
        assert!(pending_target(&config, "beta").is_err());

        // A writer never replaces a journal it cannot read.
        let guard = journal.lock(Duration::ZERO).unwrap();
        assert!(journal.upsert(&guard, prepared("alpha", "beta")).is_err());
        assert!(
            journal
                .remove(&guard, RecoveryOperation::Rename, "alpha")
                .is_err()
        );
        assert!(journal.collect_void(&guard, &config).is_err());
        assert_eq!(std::fs::read_to_string(journal.path()).unwrap(), future);
    }

    #[test]
    fn a_corrupt_journal_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let config = config_with_agents(dir.path(), &[]);
        let guard = journal.lock(Duration::ZERO).unwrap();
        for corrupt in [
            "",
            "{not json",
            r#"{"records":[]}"#,
            r#"{"schema_version":1,"records":[{"operation":"rename"}]}"#,
            r#"{"schema_version":1,"records":[{"operation":"merge","from":"a","to":"b","phase":"prepared","armed_at":"x"}]}"#,
        ] {
            std::fs::write(journal.path(), corrupt).unwrap();
            assert!(
                matches!(journal.load(), Err(JournalError::Unreadable { .. })),
                "{corrupt:?} must be unreadable"
            );
            assert!(retired_alias(&config, "a").is_err(), "{corrupt:?}");
            assert!(
                journal.upsert(&guard, prepared("alpha", "beta")).is_err(),
                "{corrupt:?} must not be overwritten"
            );
            assert_eq!(std::fs::read_to_string(journal.path()).unwrap(), corrupt);
        }

        // A directory where the journal belongs is unreadable too.
        std::fs::remove_file(journal.path()).unwrap();
        std::fs::create_dir(journal.path()).unwrap();
        assert!(matches!(
            journal.load(),
            Err(JournalError::Unreadable { .. })
        ));
    }

    #[test]
    fn lock_contention_returns_busy_after_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let held = journal.lock(Duration::ZERO).unwrap();

        let wait = Duration::from_millis(80);
        let started = Instant::now();
        let err = journal.lock(wait).unwrap_err();
        assert!(matches!(err, JournalError::Busy { .. }), "{err:?}");
        assert!(
            started.elapsed() >= wait,
            "gave up after {:?}, before the {wait:?} wait elapsed",
            started.elapsed()
        );

        // Another handle on the same directory contends for the same lock.
        let other = AgentRecoveryJournal::for_data_dir(dir.path());
        assert!(matches!(
            other.lock(Duration::ZERO),
            Err(JournalError::Busy { .. })
        ));

        drop(held);
        assert!(
            other.lock(Duration::ZERO).is_ok(),
            "dropping the guard releases the lock"
        );
    }

    #[test]
    fn a_guard_only_admits_writes_to_its_own_journal() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_journal = AgentRecoveryJournal::for_data_dir(first.path());
        let second_journal = AgentRecoveryJournal::for_data_dir(second.path());
        let guard = first_journal.lock(Duration::ZERO).unwrap();

        assert!(matches!(
            second_journal.upsert(&guard, prepared("alpha", "beta")),
            Err(JournalError::Write { .. })
        ));
        assert!(!second_journal.path().exists());
    }

    #[test]
    fn no_journal_is_written_without_a_data_dir() {
        let journal = AgentRecoveryJournal::for_data_dir(Path::new(""));
        assert!(matches!(
            journal.lock(Duration::ZERO),
            Err(JournalError::Write { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_journal_and_its_lock_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let guard = journal.lock(Duration::ZERO).unwrap();
        journal.upsert(&guard, prepared("alpha", "beta")).unwrap();

        for name in [JOURNAL_FILE_NAME, JOURNAL_LOCK_FILE_NAME] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name} mode {mode:o}");
        }
    }

    #[test]
    fn is_effective_truth_table() {
        let dir = tempfile::tempdir().unwrap();
        let has_from = config_with_agents(dir.path(), &["alpha"]);
        let has_to = config_with_agents(dir.path(), &["beta"]);
        let has_both = config_with_agents(dir.path(), &["alpha", "beta"]);
        let has_neither = config_with_agents(dir.path(), &[]);

        let pending = prepared("alpha", "beta");
        assert!(!pending.is_effective(&has_from), "commit never landed");
        assert!(
            !pending.is_effective(&has_both),
            "`from` is still configured"
        );
        assert!(pending.is_effective(&has_to), "commit landed");
        assert!(
            pending.is_effective(&has_neither),
            "`from` is gone, so it stays retired whatever became of `to`"
        );

        let landed = committed("alpha", "beta");
        for config in [&has_from, &has_to, &has_both, &has_neither] {
            assert!(landed.is_effective(config));
        }
    }

    #[test]
    fn retired_alias_and_pending_target_see_only_effective_records() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let guard = journal.lock(Duration::ZERO).unwrap();
        journal.upsert(&guard, prepared("alpha", "beta")).unwrap();
        journal.upsert(&guard, committed("gamma", "delta")).unwrap();

        // Readers never take the lock, so they answer while a writer holds it.
        let before_commit = config_with_agents(dir.path(), &["alpha", "delta"]);
        assert_eq!(retired_alias(&before_commit, "alpha").unwrap(), None);
        assert_eq!(pending_target(&before_commit, "beta").unwrap(), None);
        assert_eq!(
            retired_alias(&before_commit, "gamma").unwrap(),
            Some(committed("gamma", "delta"))
        );
        assert_eq!(
            pending_target(&before_commit, "delta").unwrap(),
            Some(committed("gamma", "delta"))
        );
        assert_eq!(retired_alias(&before_commit, "delta").unwrap(), None);
        assert_eq!(pending_target(&before_commit, "gamma").unwrap(), None);
        assert_eq!(retired_alias(&before_commit, "omega").unwrap(), None);

        let after_commit = config_with_agents(dir.path(), &["beta", "delta"]);
        assert_eq!(
            retired_alias(&after_commit, "alpha").unwrap(),
            Some(prepared("alpha", "beta"))
        );
        assert_eq!(
            pending_target(&after_commit, "beta").unwrap(),
            Some(prepared("alpha", "beta"))
        );
        drop(guard);
    }

    #[test]
    fn errors_name_the_journal_file_and_never_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let held = journal.lock(Duration::ZERO).unwrap();
        let busy = journal.lock(Duration::ZERO).unwrap_err();
        drop(held);
        std::fs::write(journal.path(), "{not json").unwrap();
        let unreadable = journal.load().unwrap_err();
        std::fs::write(journal.path(), r#"{"schema_version":2,"records":[]}"#).unwrap();
        let unsupported = journal.load().unwrap_err();
        let other = tempfile::tempdir().unwrap();
        let foreign = AgentRecoveryJournal::for_data_dir(other.path())
            .lock(Duration::ZERO)
            .unwrap();
        std::fs::remove_file(journal.path()).unwrap();
        let unwritable = journal
            .upsert(&foreign, prepared("alpha", "beta"))
            .unwrap_err();

        let install = dir.path().display().to_string();
        for (error, expected) in [
            (
                &busy,
                format!("{JOURNAL_LOCK_FILE_NAME} is locked by another agent lifecycle operation"),
            ),
            (&unreadable, format!("{JOURNAL_FILE_NAME} is unreadable: ")),
            (
                &unsupported,
                format!("{JOURNAL_FILE_NAME} uses schema_version 2; this build reads 1"),
            ),
            (&unwritable, format!("cannot write {JOURNAL_FILE_NAME}: ")),
        ] {
            let text = error.to_string();
            assert!(text.starts_with(&expected), "{text}");
            assert!(!text.contains(&install), "{text}");
            assert!(
                !text.contains(&other.path().display().to_string()),
                "{text}"
            );
        }
        // The absolute path is still there for logs.
        assert!(
            matches!(&busy, JournalError::Busy { path } if path == &dir.path().join(JOURNAL_LOCK_FILE_NAME))
        );
    }

    #[test]
    fn guards_see_no_journal_without_a_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let journal = AgentRecoveryJournal::for_data_dir(dir.path());
        let guard = journal.lock(Duration::ZERO).unwrap();
        journal.upsert(&guard, committed("gamma", "delta")).unwrap();

        let mut config = config_with_agents(dir.path(), &[]);
        config.data_dir = PathBuf::new();
        assert_eq!(retired_alias(&config, "gamma").unwrap(), None);
        assert_eq!(pending_target(&config, "delta").unwrap(), None);
    }

    #[test]
    fn journal_file_names_cover_the_journal_its_lock_and_its_temp_files() {
        assert!(is_journal_file_name(JOURNAL_FILE_NAME));
        assert!(is_journal_file_name(JOURNAL_LOCK_FILE_NAME));
        assert!(is_journal_file_name(&format!(
            "{JOURNAL_TEMP_PREFIX}{}",
            uuid::Uuid::new_v4()
        )));
        assert!(!is_journal_file_name("agent-lifecycle-recovery.json.bak"));
        assert!(!is_journal_file_name("agent-lifecycle-recovery.jsonl"));
        assert!(!is_journal_file_name("config.toml"));
    }
}
