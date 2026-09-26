//! Config-file write serialization: two locks with distinct scopes and one
//! fixed order.
//!
//! - [`shared_config_write_lock`] is the process-wide *transaction* lock. A
//!   writer takes it before its first read-for-modify and holds it through the
//!   save and any publish, so interleaved transactions cannot write each other's
//!   stale state back.
//! - [`acquire`] is the per-path *disk* lock. `Config` save methods take it
//!   internally around the final read-compare-replace of the config file, which
//!   also covers writers that never enter a transaction.
//!
//! Lock order is transaction lock first, disk lock second, always. The disk
//! lock is only ever held inside a save method, and save methods never acquire
//! the transaction lock, so the order cannot invert. Neither lock is reentrant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock, Weak};

/// The one mutex every config writer in this process serializes on.
///
/// A config write is a transaction, not a single call: a writer reads the live
/// config, mutates a copy, saves the whole snapshot to `config.toml`, then
/// publishes the result back to the shared handle. `Config::save` syncs the
/// serialized snapshot onto the existing document and drops keys the snapshot
/// does not carry, so two writers whose transactions interleave do not merge —
/// the later save writes the earlier writer's state back over the newer one,
/// and the change is lost on disk even when memory still holds it.
///
/// The gateway and the RPC path each already serialized their own handlers,
/// but on separate mutexes, and the channel pairing writer took neither. This
/// is the shared one, so a writer is safe against the others without having to
/// prove which `Arc<RwLock<Config>>` it happens to hold: several exist in a
/// running daemon (the gateway shares one with the channel supervisor, the RPC
/// context builds its own), and a per-handle lock only protects writers that
/// reached the config the same way.
///
/// A tokio mutex, not `parking_lot`, because the guard must survive the
/// `.await` on config-save I/O.
///
/// Lock order is this mutex first, the config `RwLock` second, always. Acquire
/// it before the first read-for-modify and hold it through the publish. Never
/// acquire it while holding a config guard, and never re-acquire it in a
/// callee: it is not reentrant, so a nested acquisition deadlocks.
///
/// Serializing every writer in the process is stricter than correctness
/// requires, since writers on unrelated config handles cannot actually clobber
/// each other. It costs nothing worth measuring: config writes are operator
/// edits and pairing binds, not a hot path.
#[must_use]
pub fn shared_config_write_lock() -> Arc<tokio::sync::Mutex<()>> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    Arc::clone(LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(()))))
}

static LOCKS: LazyLock<parking_lot::Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

/// Serialize a direct config-file transaction, such as schema migration.
/// `Config` save methods acquire this internally: never call them while
/// holding this non-reentrant guard.
pub async fn acquire(path: &Path) -> std::io::Result<tokio::sync::OwnedMutexGuard<()>> {
    // Normalize the parent rather than the file: atomic replacement changes
    // the inode, and first-time writers may not have a file to canonicalize.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let canonical_parent = tokio::fs::canonicalize(parent).await?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "config path has no file name",
        )
    })?;
    let key = canonical_parent.join(name);
    let lock = {
        let mut locks = LOCKS.lock();
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(key, Arc::downgrade(&lock));
            lock
        }
    };
    Ok(lock.lock_owned().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_caller_gets_the_same_mutex() {
        assert!(Arc::ptr_eq(
            &shared_config_write_lock(),
            &shared_config_write_lock()
        ));
    }

    /// The guard a writer holds is visible to every other caller, which is what
    /// makes `try_lock().is_err()` usable as a "caller holds the lock" assertion
    /// in the gateway and RPC paths.
    #[tokio::test]
    async fn a_held_guard_blocks_the_other_handles() {
        let held = shared_config_write_lock().lock_owned().await;
        assert!(shared_config_write_lock().try_lock().is_err());
        drop(held);
        assert!(shared_config_write_lock().try_lock().is_ok());
    }
}
