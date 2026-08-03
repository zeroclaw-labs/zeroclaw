//! Idempotency guard for inbound channel messages.
//!
//! # Why this exists
//!
//! WhatsApp (and most push-style channels) redeliver a message when the
//! client never acknowledged it. If the daemon crashes in the window between
//! "message received" and "reply sent", the platform hands us the exact same
//! message again on reconnect. Without a durable record of what we already
//! answered, the agent answers twice — and to the human on the other end that
//! reads as the person repeating themselves, which is precisely the seam a
//! companion agent must never show.
//!
//! Re-reading history on reconnect is *desirable* (the agent should catch up
//! on what it missed while down). What must be impossible is **acting twice on
//! the same message**. This module draws that line.
//!
//! # Why SQLite and not a JSON file
//!
//! The obvious implementation — load a `HashSet` from JSON, check, insert,
//! write back — has two fatal flaws for this use case:
//!
//! 1. **It is not crash-atomic.** A crash between the check and the write
//!    leaves the message unrecorded, which is exactly the crash we are
//!    defending against. The failure mode of the guard is identical to the
//!    failure mode it exists to prevent.
//! 2. **It is not concurrency-safe.** Two tasks processing messages
//!    simultaneously both read the same snapshot and the later write erases
//!    the earlier one.
//!
//! A `UNIQUE` constraint in SQLite makes the claim atomic at the storage
//! layer: `INSERT OR IGNORE` either inserts (we are first, proceed) or does
//! nothing (someone already claimed it, skip). The decision and the durable
//! record are the *same operation*, so there is no window to crash inside.
//! With `synchronous=FULL` the row survives power loss, not just process
//! death.
//!
//! # Ordering contract
//!
//! The claim MUST happen before the agent is invoked, never after the reply is
//! sent. Claiming after replying reopens the very window this closes. The cost
//! of claiming first is that a crash *during* generation drops that one reply
//! (the message is marked seen but never answered) — a silent miss. That is
//! the correct trade: a companion agent that occasionally misses a message
//! looks like a person who got distracted; one that answers the same message
//! twice looks like a malfunctioning bot.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// How long a processed-message record is retained.
///
/// WhatsApp will not redeliver a message older than a few days, so keeping
/// records forever only grows the file. Two weeks is comfortably beyond any
/// realistic redelivery window while keeping the table small enough that the
/// uniqueness index stays in page cache.
const RETENTION_SECS: i64 = 14 * 24 * 60 * 60;

/// Rows deleted per vacuum pass, so cleanup never blocks a message for long.
const VACUUM_BATCH: usize = 5_000;

/// Durable record of which inbound messages have already been acted on.
pub struct ProcessedMessageStore {
    conn: Mutex<Connection>,
}

impl ProcessedMessageStore {
    /// Open (creating if absent) the store at `<state_dir>/processed-messages.db`.
    pub fn open_in_state_dir(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating state dir {}", state_dir.display()))?;
        Self::open(&state_dir.join("processed-messages.db"))
    }

    /// Open (creating if absent) the store at an explicit path.
    pub fn open(path: &PathBuf) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening processed-message store {}", path.display()))?;

        // WAL: a reader (vacuum) never blocks the writer (claim).
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("setting journal_mode=WAL")?;
        // FULL, not NORMAL: this store's entire purpose is surviving an
        // unclean shutdown. NORMAL can lose the most recent commits on power
        // loss, which would resurrect the duplicate we just prevented.
        conn.pragma_update(None, "synchronous", "FULL")
            .context("setting synchronous=FULL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("setting busy_timeout")?;

        // `key` is the PRIMARY KEY: uniqueness is enforced by storage, not by
        // application logic that a crash could interrupt halfway.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS processed_messages (
                 key          TEXT PRIMARY KEY,
                 processed_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_processed_at
                 ON processed_messages(processed_at);",
        )
        .context("creating processed_messages schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Build the identity key for a message.
    ///
    /// Scoped by channel and sender as well as the platform message id: two
    /// different channels could in principle mint colliding ids, and scoping
    /// keeps one peer's traffic from ever masking another's.
    pub fn key_for(channel: &str, sender: &str, message_id: &str) -> String {
        format!("{channel}|{sender}|{message_id}")
    }

    /// Atomically claim a message for processing.
    ///
    /// Returns `true` if the caller now owns this message and should process
    /// it, `false` if it was already claimed (duplicate — skip silently).
    ///
    /// On storage failure this returns `true` (fail-open). A dead store must
    /// not render the agent mute: answering twice in a rare failure is less
    /// harmful than never answering at all. The error is surfaced to the
    /// caller for logging via [`Self::claim_with_status`].
    pub fn claim(&self, key: &str) -> bool {
        matches!(
            self.claim_with_status(key),
            ClaimOutcome::Claimed | ClaimOutcome::StoreError(_)
        )
    }

    /// Like [`Self::claim`] but reports precisely what happened, so callers
    /// can log a storage fault instead of silently degrading.
    pub fn claim_with_status(&self, key: &str) -> ClaimOutcome {
        let now = unix_now();
        let guard = match self.conn.lock() {
            Ok(g) => g,
            // A poisoned mutex means another thread panicked mid-claim. Fail
            // open rather than propagate the panic into the message loop.
            Err(poisoned) => poisoned.into_inner(),
        };

        // INSERT OR IGNORE is the whole mechanism: the uniqueness check and
        // the durable write are one atomic statement, so there is no window
        // in which we have decided to process but not yet recorded it.
        match guard.execute(
            "INSERT OR IGNORE INTO processed_messages (key, processed_at) VALUES (?1, ?2)",
            rusqlite::params![key, now],
        ) {
            Ok(1) => ClaimOutcome::Claimed,
            Ok(_) => ClaimOutcome::Duplicate,
            Err(e) => ClaimOutcome::StoreError(e.to_string()),
        }
    }

    /// Drop records older than the retention window.
    ///
    /// Deletes in bounded batches so a large backlog can't hold the write lock
    /// long enough to delay an inbound message. Returns rows removed.
    pub fn vacuum(&self) -> Result<usize> {
        let cutoff = unix_now() - RETENTION_SECS;
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let removed = guard
            .execute(
                "DELETE FROM processed_messages
                   WHERE key IN (
                     SELECT key FROM processed_messages
                      WHERE processed_at < ?1
                      LIMIT ?2
                   )",
                rusqlite::params![cutoff, VACUUM_BATCH],
            )
            .context("vacuuming processed_messages")?;
        Ok(removed)
    }

    /// Number of retained records (diagnostics only).
    pub fn len(&self) -> Result<usize> {
        let guard = match self.conn.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let n: i64 = guard
            .query_row("SELECT COUNT(*) FROM processed_messages", [], |r| r.get(0))
            .context("counting processed_messages")?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// True when no records are retained.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// Result of attempting to claim a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Caller owns this message and must process it.
    Claimed,
    /// Already processed — skip without replying.
    Duplicate,
    /// Storage failed; caller should process (fail-open) but log this.
    StoreError(String),
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (ProcessedMessageStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = ProcessedMessageStore::open_in_state_dir(dir.path()).expect("open");
        (s, dir)
    }

    #[test]
    fn first_claim_succeeds_and_second_is_refused() {
        let (s, _d) = store();
        let k = ProcessedMessageStore::key_for("whatsapp", "victoria", "MSG_A");
        assert_eq!(s.claim_with_status(&k), ClaimOutcome::Claimed);
        assert_eq!(s.claim_with_status(&k), ClaimOutcome::Duplicate);
        assert_eq!(s.claim_with_status(&k), ClaimOutcome::Duplicate);
    }

    #[test]
    fn distinct_messages_each_claim() {
        let (s, _d) = store();
        for id in ["A", "B", "C"] {
            let k = ProcessedMessageStore::key_for("whatsapp", "victoria", id);
            assert_eq!(s.claim_with_status(&k), ClaimOutcome::Claimed);
        }
        assert_eq!(s.len().unwrap(), 3);
    }

    #[test]
    fn same_id_from_different_senders_does_not_collide() {
        let (s, _d) = store();
        let a = ProcessedMessageStore::key_for("whatsapp", "victoria", "SAME");
        let b = ProcessedMessageStore::key_for("whatsapp", "otro", "SAME");
        assert_eq!(s.claim_with_status(&a), ClaimOutcome::Claimed);
        assert_eq!(s.claim_with_status(&b), ClaimOutcome::Claimed);
    }

    #[test]
    fn claim_survives_reopen() {
        // The crash case: claim, drop the handle (simulating process death),
        // reopen from disk, and confirm the record persisted.
        let dir = tempfile::tempdir().expect("tempdir");
        let k = ProcessedMessageStore::key_for("whatsapp", "victoria", "PERSISTED");
        {
            let s = ProcessedMessageStore::open_in_state_dir(dir.path()).expect("open");
            assert_eq!(s.claim_with_status(&k), ClaimOutcome::Claimed);
        }
        let reopened = ProcessedMessageStore::open_in_state_dir(dir.path()).expect("reopen");
        assert_eq!(
            reopened.claim_with_status(&k),
            ClaimOutcome::Duplicate,
            "a message claimed before a crash must stay claimed after restart"
        );
    }

    #[test]
    fn concurrent_claims_elect_exactly_one_winner() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Arc::new(ProcessedMessageStore::open_in_state_dir(dir.path()).expect("open"));
        let k = ProcessedMessageStore::key_for("whatsapp", "victoria", "RACE");

        let winners: Vec<_> = (0..16)
            .map(|_| {
                let s = Arc::clone(&s);
                let k = k.clone();
                std::thread::spawn(move || s.claim_with_status(&k) == ClaimOutcome::Claimed)
            })
            .map(|h| h.join().expect("thread"))
            .collect();

        assert_eq!(
            winners.iter().filter(|w| **w).count(),
            1,
            "exactly one racing task may claim a given message"
        );
    }

    #[test]
    fn vacuum_removes_only_expired_records() {
        let (s, _d) = store();
        let fresh = ProcessedMessageStore::key_for("whatsapp", "victoria", "FRESH");
        assert_eq!(s.claim_with_status(&fresh), ClaimOutcome::Claimed);

        // Backdate one record beyond the retention window.
        {
            let g = s.conn.lock().unwrap();
            g.execute(
                "INSERT INTO processed_messages (key, processed_at) VALUES (?1, ?2)",
                rusqlite::params!["whatsapp|victoria|OLD", unix_now() - RETENTION_SECS - 1],
            )
            .unwrap();
        }
        assert_eq!(s.len().unwrap(), 2);
        assert_eq!(s.vacuum().unwrap(), 1);
        assert_eq!(s.len().unwrap(), 1);
        // The fresh record must still block a duplicate after vacuuming.
        assert_eq!(s.claim_with_status(&fresh), ClaimOutcome::Duplicate);
    }

    #[test]
    fn claim_fails_open_so_a_broken_store_never_mutes_the_agent() {
        let (s, _d) = store();
        // Drop the table out from under the store to simulate corruption.
        {
            let g = s.conn.lock().unwrap();
            g.execute_batch("DROP TABLE processed_messages").unwrap();
        }
        let k = ProcessedMessageStore::key_for("whatsapp", "victoria", "BROKEN");
        assert!(
            matches!(s.claim_with_status(&k), ClaimOutcome::StoreError(_)),
            "a storage fault must be reported, not silently swallowed"
        );
        assert!(
            s.claim(&k),
            "a broken store must fail open: replying twice beats never replying"
        );
    }
}
