use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock};

use super::shadow::ShadowSnapshot;

/// Process-global registry of `ShadowSnapshot` instances, keyed by the
/// canonical worktree path. Sharing by worktree ensures concurrent sessions
/// on the same directory use the same shadow repo and lock.
///
/// `RwLock<HashMap>` is used rather than `DashMap` to avoid an extra dep —
/// the registry sees per-session writes (rare) and per-tool-call reads
/// (frequent but uncontended once warm).
static REGISTRY: LazyLock<RwLock<HashMap<String, Arc<ShadowSnapshot>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Return the `ShadowSnapshot` for `working_dir`, creating it on first call.
/// Returns `None` when git is unavailable or the directory is not in a repo.
///
/// `data_dir` is the snapshot storage root (typically `config.data_dir`).
pub fn get_or_create(working_dir: &Path, data_dir: &Path) -> Option<Arc<ShadowSnapshot>> {
    let key = canonical_key(working_dir);
    {
        let read = REGISTRY.read().ok()?;
        if let Some(existing) = read.get(&key) {
            return Some(existing.clone());
        }
    }
    let snap = ShadowSnapshot::for_session(working_dir, data_dir)?;
    let arc = Arc::new(snap);
    let mut write = REGISTRY.write().ok()?;
    // Re-check under the write lock to avoid clobbering a concurrent insert.
    Some(write.entry(key).or_insert(arc).clone())
}

/// Drop the cached snapshot for `working_dir` (e.g. when a session ends).
pub fn remove(working_dir: &Path) {
    let key = canonical_key(working_dir);
    if let Ok(mut write) = REGISTRY.write() {
        write.remove(&key);
    }
}

fn canonical_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_on_unknown_key_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let unknown = tmp.path().join("never-inserted");
        // Must not panic, must not poison the lock.
        remove(&unknown);
        assert!(
            REGISTRY
                .read()
                .unwrap()
                .get(&canonical_key(&unknown))
                .is_none()
        );
    }

    #[test]
    fn get_or_create_returns_none_when_not_in_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("nogit");
        std::fs::create_dir_all(&nested).unwrap();
        let data = tempfile::tempdir().unwrap();
        // No .git anywhere above `nested` → ShadowSnapshot::for_session returns None.
        assert!(get_or_create(&nested, data.path()).is_none());
    }
}
