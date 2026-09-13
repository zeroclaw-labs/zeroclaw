//! Process-global cache for skill-directory loads.

use super::{DroppedSkill, Skill};
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

#[derive(Clone)]
pub(super) struct LoadOutput {
    pub skills: Vec<Skill>,
    pub dropped: Vec<DroppedSkill>,
}

struct CacheEntry {
    signature: u64,
    output: LoadOutput,
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
            } else if file_type.is_file() {
                // Decline to cache rather than fingerprint a file we can't read.
                let digest = hash_file_fingerprint(&path)?;
                entries.insert(path, (1, digest));
            } else {
                return None;
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

/// Fingerprint one file for the directory signature.
///
/// How much of a file has to be digested follows from how much of it can
/// change a load decision, and those are not the same for every file. The
/// audit parses markdown and TOML in full, and the loader reads the manifest
/// and the skill body out of them, so their whole contents count. Every other
/// file is only ever inspected for a leading shebang
/// ([`audit::SHEBANG_SNIFF_BYTES`]); nothing on the load path reads further
/// into one. Digesting a bundled model or image in full would therefore make
/// cache validation scale with payload size while distinguishing nothing.
///
/// Length is folded in for the sniffed files so truncation past the prefix is
/// still visible, which costs a `stat` the walk has already done.
///
/// This is not a weakening of the freshness contract: it computes the same
/// function of the inputs that actually feed a load decision. If a file type
/// gains a full-content reader on the load path, it belongs in
/// [`audit::audit_reads_full_contents`] and this follows automatically.
fn hash_file_fingerprint(path: &Path) -> Option<u64> {
    if super::audit::audit_reads_full_contents(path) {
        return hash_file_contents(path);
    }
    hash_file_prefix(path)
}

/// Hash the leading [`audit::SHEBANG_SNIFF_BYTES`] of a file, plus its length.
fn hash_file_prefix(path: &Path) -> Option<u64> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut prefix = Vec::with_capacity(super::audit::SHEBANG_SNIFF_BYTES);
    Read::take(file, super::audit::SHEBANG_SNIFF_BYTES as u64)
        .read_to_end(&mut prefix)
        .ok()?;

    let mut hasher = DefaultHasher::new();
    prefix.hash(&mut hasher);
    len.hash(&mut hasher);
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

pub(super) fn cached_load(
    dir: &Path,
    allow_scripts: bool,
    tag: &'static str,
    load: impl FnOnce() -> LoadOutput,
) -> LoadOutput {
    cached_load_in(cache(), dir, allow_scripts, tag, load)
}

fn cached_load_in(
    cache: &RwLock<HashMap<CacheKey, CacheEntry>>,
    dir: &Path,
    allow_scripts: bool,
    tag: &'static str,
    load: impl FnOnce() -> LoadOutput,
) -> LoadOutput {
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
        let guard = cache.read().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = guard.get(&key)
            && entry.signature == signature
        {
            return entry.output.clone();
        }
    }

    // Miss: load outside the write lock would be cleaner, but the loader is fast
    // relative to lock contention here and we want a single store. If the dir
    // mutates during `load`, its content digest changes, so the *next* call's
    // signature differs from what we store and the entry self-heals.
    let output = load();
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    guard.insert(
        key,
        CacheEntry {
            signature,
            output: output.clone(),
        },
    );
    output
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
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);

        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: vec![Skill {
                    name: "alpha".into(),
                    description: String::new(),
                    description_localizations: Default::default(),
                    version: String::new(),
                    author: None,
                    tags: vec![],
                    tools: vec![],
                    prompts: vec![],
                    slash_options: vec![],
                    always: false,
                    location: None,
                }],
                dropped: vec![],
            }
        };

        let a = cached_load_in(&local_cache, &skills_dir, false, "test", load);
        let b = cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(a.skills.len(), 1);
        assert_eq!(b.skills.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "loader should run once");
    }

    #[test]
    fn adding_a_skill_invalidates_via_signature() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        write(&skills_dir, "beta", "# Beta\n");
        cached_load_in(&local_cache, &skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "adding a skill dir must bust the cache"
        );
    }

    #[test]
    fn editing_content_invalidates_via_signature() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        // Different length -> signature changes even if mtime resolution is coarse.
        write(
            &skills_dir,
            "alpha",
            "# Alpha skill, now with a longer body.\n",
        );
        cached_load_in(&local_cache, &skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "editing skill content must bust the cache"
        );
    }

    /// A bundled asset's body is not an input to any load decision, so
    /// digesting it in full would make cache validation scale with payload
    /// size while distinguishing nothing. Editing past the sniffed prefix must
    /// therefore be a cache hit.
    #[test]
    fn edits_past_the_sniffed_prefix_of_an_asset_do_not_bust_the_cache() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "body\n");

        // Same length, differing only well past the shebang window.
        let asset = skills_dir.join("alpha/model.bin");
        let mut first = vec![b'A'; 4096];
        first[2048] = b'X';
        std::fs::write(&asset, &first).unwrap();

        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut second = vec![b'A'; 4096];
        second[2048] = b'Y';
        std::fs::write(&asset, &second).unwrap();

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a change no load decision can observe must not force a reload"
        );
    }

    /// The other half of that trade. The audit sniffs the leading bytes of
    /// every file for a shebang, so a benign asset turning into a script is a
    /// verdict change and must be seen.
    #[test]
    fn gaining_a_shebang_inside_the_prefix_busts_the_cache() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "body\n");

        // Equal-length payloads that differ only inside the sniffed prefix, so
        // the reload can come from nothing but the prefix bytes flipping from
        // benign content to a shell shebang. `hash_file_prefix` also folds the
        // file length into the fingerprint, so a length change alone would bust
        // the cache and this test would pass even if the prefix were ignored.
        let benign = b"plain data, nothing executable in this file\n";
        let mut script = b"#!/bin/sh\necho hi\n".to_vec();
        while script.len() < benign.len() {
            script.push(b'#'); // filler comment bytes, still within the 128-byte prefix
        }
        assert_eq!(
            benign.len(),
            script.len(),
            "fixtures must be equal length so only the prefix content differs"
        );
        assert!(
            script.len() <= crate::skills::audit::SHEBANG_SNIFF_BYTES,
            "both payloads must fit inside the sniffed prefix window"
        );

        let asset = skills_dir.join("alpha/payload");
        std::fs::write(&asset, benign).unwrap();

        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::fs::write(&asset, &script).unwrap();

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a file becoming script-like changes the audit verdict and must reload"
        );
    }

    /// Truncation or extension is visible even when the sniffed prefix is
    /// identical, since length is folded into the fingerprint.
    #[test]
    fn changing_only_length_past_the_prefix_busts_the_cache() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "body\n");

        let asset = skills_dir.join("alpha/blob.bin");
        std::fs::write(&asset, vec![b'Z'; 4096]).unwrap();

        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::fs::write(&asset, vec![b'Z'; 8192]).unwrap();

        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn same_length_same_mtime_edit_still_busts_cache() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "AAAA\n");
        let skill_md = skills_dir.join("alpha/SKILL.md");
        let original_mtime =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&skill_md).unwrap());

        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        cached_load_in(&local_cache, &skills_dir, false, "test", load);

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

        cached_load_in(&local_cache, &skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "content change under identical mtime+length must re-audit"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_entry_bypasses_cache_without_hanging() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let fifo = skills_dir.join("alpha/pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo should be available on unix test hosts");
        assert!(status.success(), "mkfifo failed");

        let calls = AtomicUsize::new(0);
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };

        // Must return promptly (no hang) and, because the dir can't be signed,
        // run the loader every time instead of caching.
        cached_load_in(&local_cache, &skills_dir, false, "test", load);
        cached_load_in(&local_cache, &skills_dir, false, "test", load);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a directory containing a non-regular entry must bypass the cache"
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
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
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
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
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
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
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

    #[test]
    fn dropped_records_survive_cache_hit() {
        let local_cache = RwLock::new(HashMap::new());
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        write(&skills_dir, "alpha", "# Alpha\n");
        let calls = AtomicUsize::new(0);

        let drop = || DroppedSkill {
            name: "bad".into(),
            origin_hint: "workspace".into(),
            reason: super::super::SkillDropReason::AuditError("boom".into()),
            location: None,
        };
        let load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![drop()],
            }
        };

        let first = cached_load_in(&local_cache, &skills_dir, false, "test", load);
        // On the hit the loader must NOT run; the closure asserts via call count.
        let hit_load = || {
            calls.fetch_add(1, Ordering::SeqCst);
            LoadOutput {
                skills: Vec::new(),
                dropped: vec![],
            }
        };
        let second = cached_load_in(&local_cache, &skills_dir, false, "test", hit_load);

        assert_eq!(first.dropped.len(), 1);
        assert_eq!(
            second.dropped.len(),
            1,
            "drops must survive the cache hit, not be recomputed"
        );
        assert_eq!(second.dropped[0].name, "bad");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the loader must run only on the miss"
        );
    }
}
