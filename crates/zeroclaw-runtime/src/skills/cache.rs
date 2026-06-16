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
//! and validates freshness with a **content digest** of the directory: it hashes
//! the bytes of every file reachable under `dir` (plus each symlink's target),
//! never following symlinks so it can't loop. Because the digest covers file
//! *content*, any change the auditor would care about — an edited `SKILL.md`, a
//! flipped script, a retargeted symlink, altered TOML — produces a different
//! signature and forces a re-audit. This matters specifically because the cache
//! sits in front of the security audit: serving a cached "clean" verdict for
//! content that has since changed would defeat the audit, so the freshness key is
//! deliberately tied to the audited bytes rather than to metadata (mtime/length),
//! which an edit can preserve. (The only residual risk is a 64-bit hash
//! collision, which is not a practical forgery vector.)
//!
//! The digest reads each file once, but a cache *hit* then skips the audit's
//! content scan, its regex/script/symlink checks, and the Markdown/TOML parsing —
//! work the loader otherwise repeats (re-reading files) on every prompt build and
//! every `read_skill` call. So the cache stays a net win without weakening the
//! audit boundary.
//!
//! [`invalidate`] gives the [`super::SkillsService`] an explicit hook to drop the
//! cache immediately after a write, so an added/edited/removed skill is picked up
//! on the very next load without waiting on anything.
//!
//! Kill-switch: the cache is on by default; setting `ZEROCLAW_SKILLS_CACHE_ENABLED`
//! to a falsey value (`0` / `false` / `no` / `off`) forces every load to re-walk
//! and re-audit, i.e. the exact pre-cache behavior. This is a runtime off-ramp if
//! the cache is ever suspected of serving stale results.

use super::Skill;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

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

/// Content fingerprint of everything reachable under `dir` (recursive). Hashes
/// each entry's path plus a digest of its *bytes* (files) or link target
/// (symlinks). Never follows symlinks, so it can't loop on a cycle and matches
/// the auditor's no-follow stance. Tying the key to content — not metadata an edit
/// can preserve — is what keeps a cached "clean" audit verdict from outliving the
/// bytes it audited. Returns `None` when `dir` is absent or any entry can't be
/// read; callers treat that as "do not cache" rather than trust a partial digest.
fn dir_signature(dir: &Path) -> Option<u64> {
    if !dir.exists() {
        return None;
    }

    // BTreeMap keyed by path → deterministic hash order regardless of read_dir
    // ordering. Value: (kind, content-or-target digest).
    let mut entries: BTreeMap<PathBuf, (u8, u64)> = BTreeMap::new();
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
                let target = std::fs::read_link(&path).ok()?;
                let mut h = DefaultHasher::new();
                target.hash(&mut h);
                entries.insert(path, (2, h.finish()));
            } else if file_type.is_dir() {
                stack.push(path);
            } else {
                // Decline to cache rather than fingerprint a file we can't read.
                let digest = hash_file_contents(&path)?;
                entries.insert(path, (1, digest));
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

/// Stream a file's full contents through a hasher (chunked, so a large bundled
/// asset doesn't get slurped whole). `None` on any read error — the caller then
/// declines to cache instead of trusting an incomplete digest.
fn hash_file_contents(path: &Path) -> Option<u64> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = DefaultHasher::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.write(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
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
    // mutates during `load`, its content digest changes, so the *next* call's
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

    // Audit-boundary regression (review of #7786): the cache sits in front of the
    // security audit, so an edit that preserves BOTH length and mtime — exactly the
    // case a metadata-only signature would miss — must still force a re-audit. This
    // would fail on the original mtime+length signature and passes because the key
    // is now a content digest.
    #[test]
    fn same_length_same_mtime_edit_still_busts_cache() {
        invalidate();
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "AAAA\n");
        let skill_md = skills_dir.join("alpha/SKILL.md");
        let original_mtime =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&skill_md).unwrap());

        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            Vec::<Skill>::new()
        };

        cached_load(&skills_dir, false, "test", load);

        // Rewrite with same byte length, then forcibly restore the original mtime
        // so length + mtime are byte-for-byte identical to the cached state.
        std::fs::write(&skill_md, "BBBB\n").unwrap();
        filetime::set_file_mtime(&skill_md, original_mtime).unwrap();
        let after =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&skill_md).unwrap());
        assert_eq!(after, original_mtime, "test precondition: mtime restored");
        assert_eq!(
            std::fs::metadata(&skill_md).unwrap().len(),
            5,
            "test precondition: length unchanged"
        );

        cached_load(&skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "content change under identical mtime+length must re-audit"
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
