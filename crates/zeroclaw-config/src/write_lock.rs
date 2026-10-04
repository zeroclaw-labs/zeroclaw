//! The process-wide mutex that serializes config read-modify-save-publish.

use std::sync::{Arc, OnceLock};

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
