//! Process-global cache for skill-directory loads.
//!
//! [`super::load_skills_from_directory`] and [`super::load_open_skills_from_directory`]
//! are pure functions of `(dir, allow_scripts, filesystem state)`, but each call
//! does a recursive read *and* a full security audit (content scan + parse) of
//! every skill subdirectory. They run on every prompt build and every
//! `read_skill` invocation, so the cost recurs constantly even when nothing on
//! disk has changed.
//!
//! This module memoizes the result keyed by `(canonical dir, allow_scripts, tag)`
//! and validates freshness with a cheap, **stat-only** directory signature: any
//! add / remove / rename / content edit changes a file's mtime or length (or a
//! symlink's target), which changes the signature and forces a re-audit. A stale
//! "clean" audit verdict can therefore never be served from cache.
//!
//! [`invalidate`] gives the [`super::SkillsService`] an explicit hook to drop the
//! cache immediately after a write, so an added/edited/removed skill is picked up
//! on the very next load without waiting on anything.
//!
//! Kill-switch: the cache is on by default; setting `ZEROCLAW_SKILLS_CACHE_ENABLED`
//! to a falsey value (`0` / `false` / `no` / `off`) forces every load to re-walk
//! and re-audit, i.e. the exact pre-cache behavior. This is a runtime off-ramp if
//! the cache is ever suspected of serving stale results.
//!
//! Caveat: the signature trusts filesystem metadata, so a deliberate edit that
//! preserves both mtime and length (e.g. an attacker resetting mtime via
//! `utimes`) would not be detected. This matches the staleness model of build
//! tools like `cargo`/`make`; anyone with write access to the skills directory
//! already controls which skills exist, so the audit is a guard against
//! accidental/unreviewed content rather than against an attacker who can forge
//! inode metadata.

use super::Skill;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::UNIX_EPOCH;

#[derive(PartialEq, Eq, Hash, Clone)]
struct CacheKey {
    dir: PathBuf,
    allow_scripts: bool,
    /// Distinguishes loaders that may share a directory path (workspace vs
    /// open-skills) so their cached entries never collide.
    tag: &'static str,
}

struct CacheEntry {
    signature: u64,
    skills: Vec<Skill>,
}

fn cache() -> &'static RwLock<HashMap<CacheKey, CacheEntry>> {
    static CACHE: OnceLock<RwLock<HashMap<CacheKey, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Best-effort canonicalization so two spellings of the same directory share an
/// entry. Falls back to the path as given when the dir can't be canonicalized.
fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

const CACHE_ENABLED_ENV: &str = "ZEROCLAW_SKILLS_CACHE_ENABLED";

/// Pure kill-switch decision split from the env read so it stays testable
/// without mutating process-global state. The cache is enabled unless the value
/// is explicitly falsey; unset or unrecognized values leave it enabled.
fn cache_enabled_from_env(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// Runtime kill-switch read per call (negligible beside the fs work it guards),
/// so it takes effect without a rebuild. See [`CACHE_ENABLED_ENV`].
fn cache_enabled() -> bool {
    cache_enabled_from_env(std::env::var(CACHE_ENABLED_ENV).ok().as_deref())
}

/// Stat-only fingerprint of everything reachable under `dir` (recursive). Hashes
/// each entry's relative path plus the cheap metadata that changes on any real
/// edit — file mtime + length, and symlink target. Never follows symlinks, so it
/// cannot loop on a cycle and matches the auditor's no-follow stance. Returns
/// `None` when `dir` is absent or unreadable; callers treat that as "do not
/// cache" and just run the loader (which yields an empty result anyway).
fn dir_signature(dir: &Path) -> Option<u64> {
    if !dir.exists() {
        return None;
    }

    // BTreeMap keyed by path → deterministic hash order regardless of read_dir
    // ordering. Value encodes the per-entry fingerprint.
    let mut entries: BTreeMap<PathBuf, (u8, u64, u64)> = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let read = std::fs::read_dir(&current).ok()?;
        for entry in read.flatten() {
            let path = entry.path();
            // DirEntry::file_type does not follow symlinks.
            let Ok(file_type) = entry.file_type() else {
                return None;
            };

            if file_type.is_symlink() {
                // Hash the link target string; a retargeted symlink is a change.
                let target_hash = std::fs::read_link(&path)
                    .ok()
                    .map(|t| {
                        let mut h = DefaultHasher::new();
                        t.hash(&mut h);
                        h.finish()
                    })
                    .unwrap_or(0);
                entries.insert(path, (2, target_hash, 0));
            } else if file_type.is_dir() {
                stack.push(path);
            } else {
                // DirEntry::metadata does not follow symlinks.
                let Ok(meta) = entry.metadata() else {
                    return None;
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                entries.insert(path, (1, mtime, meta.len()));
            }
        }
    }

    let mut hasher = DefaultHasher::new();
    for (path, fingerprint) in &entries {
        path.hash(&mut hasher);
        fingerprint.hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// Memoize `load` for `(dir, allow_scripts, tag)`, validated by the directory
/// signature. On a hit with a matching signature, returns a clone of the cached
/// skills without touching the auditor. On a miss (or when the directory can't be
/// signed) runs `load` and stores the result. Concurrent misses simply run the
/// idempotent loader more than once; lock poisoning is recovered, not panicked.
pub(super) fn cached_load(
    dir: &Path,
    allow_scripts: bool,
    tag: &'static str,
    load: impl FnOnce() -> Vec<Skill>,
) -> Vec<Skill> {
    if !cache_enabled() {
        return load();
    }
    let Some(signature) = dir_signature(dir) else {
        return load();
    };
    let key = CacheKey {
        dir: canonical(dir),
        allow_scripts,
        tag,
    };

    {
        let guard = cache().read().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.get(&key)
            && entry.signature == signature
        {
            return entry.skills.clone();
        }
    }

    // Miss: load outside the write lock would be cleaner, but the loader is fast
    // relative to lock contention here and we want a single store. If the dir
    // mutates during `load`, the change bumps mtime/len so the *next* call's
    // signature differs from what we store and the entry self-heals.
    let skills = load();
    let mut guard = cache().write().unwrap_or_else(|e| e.into_inner());
    guard.insert(
        key,
        CacheEntry {
            signature,
            skills: skills.clone(),
        },
    );
    skills
}

/// Drop every cached entry. Call after any out-of-band mutation of a skills
/// directory (e.g. [`super::SkillsService`] writes/removes) so the change is
/// reflected on the next load even before mtimes are re-examined.
pub fn invalidate() {
    cache().write().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn second_load_is_a_cache_hit() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);

        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            vec![Skill {
                name: "alpha".into(),
                description: String::new(),
                version: String::new(),
                author: None,
                tags: vec![],
                tools: vec![],
                prompts: vec![],
                location: None,
            }]
        };

        let a = cached_load(&skills_dir, false, "test", load);
        let b = cached_load(&skills_dir, false, "test", load);
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "loader should run once");
    }

    #[test]
    fn adding_a_skill_invalidates_via_signature() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Vec::<Skill>::new()
        };

        cached_load(&skills_dir, false, "test", load);
        write(&skills_dir, "beta", "# Beta\n");
        cached_load(&skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "adding a skill dir must bust the cache"
        );
    }

    #[test]
    fn editing_content_invalidates_via_signature() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Vec::<Skill>::new()
        };

        cached_load(&skills_dir, false, "test", load);
        // Different length → signature changes even if mtime resolution is coarse.
        write(
            &skills_dir,
            "alpha",
            "# Alpha skill, now with a longer body.\n",
        );
        cached_load(&skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "editing skill content must bust the cache"
        );
    }

    #[test]
    fn explicit_invalidate_forces_reload() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Vec::<Skill>::new()
        };

        cached_load(&skills_dir, false, "test", load);
        invalidate();
        cached_load(&skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "invalidate() must force the next load to re-run"
        );
    }

    #[test]
    fn allow_scripts_flag_is_part_of_the_key() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Vec::<Skill>::new()
        };

        cached_load(&skills_dir, false, "test", load);
        cached_load(&skills_dir, true, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "different allow_scripts must not share a cache entry"
        );
    }

    #[test]
    fn missing_dir_is_not_cached() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let absent = tmp.path().join("does-not-exist");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Vec::<Skill>::new()
        };

        cached_load(&absent, false, "test", load);
        cached_load(&absent, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "absent directory should bypass the cache entirely"
        );
    }

    #[test]
    fn kill_switch_parsing() {
        // Default (unset) → enabled.
        assert!(cache_enabled_from_env(None));
        // Falsey spellings → disabled.
        for v in ["0", "false", "no", "off", "OFF", "  False  "] {
            assert!(!cache_enabled_from_env(Some(v)), "{v:?} should disable");
        }
        // Truthy / unrecognized → enabled (fail safe to caching on).
        for v in ["1", "true", "yes", "on", "", "garbage"] {
            assert!(cache_enabled_from_env(Some(v)), "{v:?} should stay enabled");
        }
    }
}
