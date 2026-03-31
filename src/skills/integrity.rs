//! Skill-file integrity verification via SHA-256 lockfile.
//!
//! Each skill's manifest file (SKILL.md or SKILL.toml) is hashed on install or
//! explicit `hrafn skills lock`. On load, the hash is verified against the
//! lockfile (`.claude/skills/skills.lock`). A mismatch causes the skill to be
//! refused with a user-visible warning.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Filename used for the skills lockfile.
const LOCKFILE_NAME: &str = "skills.lock";

/// A single entry in the skills lockfile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockEntry {
    /// Relative path from the skills directory to the manifest file.
    pub path: String,
    /// Hex-encoded SHA-256 digest of the file contents at lock time.
    pub sha256: String,
}

/// The complete lockfile: a map from skill name to lock entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillsLockfile {
    /// Ordered map so the serialized output is deterministic.
    pub skills: BTreeMap<String, LockEntry>,
}

/// Result of verifying a single skill against the lockfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// Hash matches the lockfile.
    Ok,
    /// Skill is not present in the lockfile.
    NotLocked,
    /// Hash does not match the lockfile entry.
    Mismatch { expected: String, actual: String },
}

/// Compute the SHA-256 hex digest of a byte slice.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Resolve the path to the lockfile for a given skills directory.
pub fn lockfile_path(skills_dir: &Path) -> PathBuf {
    skills_dir.join(LOCKFILE_NAME)
}

/// Read and parse the lockfile. Returns an empty lockfile if it does not exist.
pub fn read_lockfile(skills_dir: &Path) -> Result<SkillsLockfile> {
    let path = lockfile_path(skills_dir);
    if !path.exists() {
        return Ok(SkillsLockfile::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read lockfile at {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse lockfile at {}", path.display()))
}

/// Write the lockfile to disk.
pub fn write_lockfile(skills_dir: &Path, lockfile: &SkillsLockfile) -> Result<()> {
    let path = lockfile_path(skills_dir);
    let content = serde_json::to_string_pretty(lockfile).context("failed to serialize lockfile")?;
    fs::write(&path, content.as_bytes())
        .with_context(|| format!("failed to write lockfile at {}", path.display()))
}

/// Hash and record a single skill's manifest file into the lockfile.
///
/// `manifest_path` is the absolute path to SKILL.md or SKILL.toml.
/// `skills_dir` is used to compute the relative path stored in the lockfile.
pub fn lock_skill(
    lockfile: &mut SkillsLockfile,
    skill_name: &str,
    manifest_path: &Path,
    skills_dir: &Path,
) -> Result<()> {
    let content = fs::read(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let hash = sha256_hex(&content);
    let relative = manifest_path
        .strip_prefix(skills_dir)
        .unwrap_or(manifest_path);
    lockfile.skills.insert(
        skill_name.to_string(),
        LockEntry {
            path: relative.to_string_lossy().into_owned(),
            sha256: hash,
        },
    );
    Ok(())
}

/// Verify a skill's manifest file against the lockfile.
pub fn verify_skill(
    lockfile: &SkillsLockfile,
    skill_name: &str,
    manifest_path: &Path,
) -> Result<VerifyResult> {
    let entry = match lockfile.skills.get(skill_name) {
        Some(e) => e,
        None => return Ok(VerifyResult::NotLocked),
    };
    let content = fs::read(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let actual_hash = sha256_hex(&content);
    if actual_hash == entry.sha256 {
        Ok(VerifyResult::Ok)
    } else {
        Ok(VerifyResult::Mismatch {
            expected: entry.sha256.clone(),
            actual: actual_hash,
        })
    }
}

/// Scan an entire skills directory and generate a fresh lockfile from current
/// file contents. This is the implementation behind `hrafn skills lock`.
pub fn lock_all_skills(skills_dir: &Path) -> Result<SkillsLockfile> {
    let mut lockfile = SkillsLockfile::default();
    if !skills_dir.exists() {
        return Ok(lockfile);
    }

    for entry in fs::read_dir(skills_dir)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };

        // Prefer SKILL.toml, fall back to SKILL.md (same order as load)
        let manifest = path.join("SKILL.toml");
        let manifest = if manifest.exists() {
            manifest
        } else {
            let md = path.join("SKILL.md");
            if md.exists() {
                md
            } else {
                continue;
            }
        };

        lock_skill(&mut lockfile, &skill_name, &manifest, skills_dir)?;
    }

    Ok(lockfile)
}

/// Verify all locked skills against the current lockfile.
/// Returns a list of `(skill_name, VerifyResult)` for every entry in the
/// lockfile.
pub fn verify_all_skills(skills_dir: &Path) -> Result<Vec<(String, VerifyResult)>> {
    let lockfile = read_lockfile(skills_dir)?;
    let mut results = Vec::new();

    for (name, entry) in &lockfile.skills {
        let manifest_path = skills_dir.join(&entry.path);
        if !manifest_path.exists() {
            results.push((
                name.clone(),
                VerifyResult::Mismatch {
                    expected: entry.sha256.clone(),
                    actual: "<file missing>".to_string(),
                },
            ));
            continue;
        }
        let result = verify_skill(&lockfile, name, &manifest_path)?;
        results.push((name.clone(), result));
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sha256_hex_deterministic() {
        let hash1 = sha256_hex(b"hello world");
        let hash2 = sha256_hex(b"hello world");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // 256 bits = 64 hex chars
    }

    #[test]
    fn sha256_hex_different_inputs() {
        let hash1 = sha256_hex(b"hello");
        let hash2 = sha256_hex(b"world");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn lock_and_verify_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path();
        let skill_dir = skills_dir.join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        let manifest = skill_dir.join("SKILL.md");
        fs::write(&manifest, b"# Test Skill\nHello").unwrap();

        // Lock
        let mut lockfile = SkillsLockfile::default();
        lock_skill(&mut lockfile, "test-skill", &manifest, skills_dir).unwrap();
        assert!(lockfile.skills.contains_key("test-skill"));

        // Verify passes
        let result = verify_skill(&lockfile, "test-skill", &manifest).unwrap();
        assert_eq!(result, VerifyResult::Ok);

        // Tamper and verify fails
        fs::write(&manifest, b"# Tampered\nEvil instructions").unwrap();
        let result = verify_skill(&lockfile, "test-skill", &manifest).unwrap();
        assert!(matches!(result, VerifyResult::Mismatch { .. }));
    }

    #[test]
    fn verify_unlocked_skill() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("SKILL.md");
        fs::write(&manifest, b"# Test").unwrap();

        let lockfile = SkillsLockfile::default();
        let result = verify_skill(&lockfile, "unknown", &manifest).unwrap();
        assert_eq!(result, VerifyResult::NotLocked);
    }

    #[test]
    fn lock_all_skills_scans_directory() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path();

        // Create two skills
        let skill_a = skills_dir.join("alpha");
        fs::create_dir_all(&skill_a).unwrap();
        fs::write(skill_a.join("SKILL.md"), b"# Alpha").unwrap();

        let skill_b = skills_dir.join("beta");
        fs::create_dir_all(&skill_b).unwrap();
        fs::write(
            skill_b.join("SKILL.toml"),
            b"[skill]\nname = \"beta\"\ndescription = \"B\"\n",
        )
        .unwrap();

        let lockfile = lock_all_skills(skills_dir).unwrap();
        assert_eq!(lockfile.skills.len(), 2);
        assert!(lockfile.skills.contains_key("alpha"));
        assert!(lockfile.skills.contains_key("beta"));
    }

    #[test]
    fn write_and_read_lockfile_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path();

        let mut lockfile = SkillsLockfile::default();
        lockfile.skills.insert(
            "test".to_string(),
            LockEntry {
                path: "test/SKILL.md".to_string(),
                sha256: "abc123".to_string(),
            },
        );

        write_lockfile(skills_dir, &lockfile).unwrap();
        let loaded = read_lockfile(skills_dir).unwrap();
        assert_eq!(loaded.skills.len(), 1);
        assert_eq!(loaded.skills["test"].sha256, "abc123");
    }

    #[test]
    fn read_lockfile_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let lockfile = read_lockfile(dir.path()).unwrap();
        assert!(lockfile.skills.is_empty());
    }
}
