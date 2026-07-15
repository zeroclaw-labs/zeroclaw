//! Skill install receipts: honest provenance (task 2A).
//!
//! A receipt records what was installed, from where, and what screening saw,
//! so `zeroclaw skills verify` can detect an upstream content swap or an
//! accidental local edit by recomputing the content tree hash.
//!
//! # Threat model
//!
//! Receipts detect upstream content swaps and accidental local edits. They are
//! **NOT** tamper-proof against a same-user process with write access to the
//! state directory. Storing them in a daemon-owned state directory outside the
//! agent-writable workspace raises the bar for a workspace-sandboxed agent, but
//! it is not a cryptographic guarantee — a process running as the operator can
//! rewrite both a skill and its receipt.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::SkillSource;
use super::audit::collect_paths_depth_first;
use super::screening::ScreeningReport;

/// Receipt schema version. Bump on any field change.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// Tree-hash scheme version. Bump if the hashing construction below changes.
pub const TREE_HASH_SCHEME: u32 = 1;

/// Typed, sanitized mirror of [`SkillSource`] persisted in a receipt. URLs are
/// stored with any credentials/query stripped — never "as typed" [R5].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillSourceRecord {
    ClawHub {
        slug: String,
    },
    Git {
        url: String,
    },
    Registry {
        registry: Option<String>,
        skill: String,
    },
    Local {
        path: String,
    },
}

impl SkillSourceRecord {
    /// Build a sanitized record from the typed install source.
    pub fn from_source(source: &SkillSource) -> Self {
        match source {
            SkillSource::ClawHub { slug } => Self::ClawHub { slug: slug.clone() },
            SkillSource::Git { url } => Self::Git {
                url: sanitize_git_url(url),
            },
            SkillSource::Registry { registry, skill } => Self::Registry {
                registry: registry.clone(),
                skill: skill.clone(),
            },
            SkillSource::Local { path } => Self::Local {
                path: path.display().to_string(),
            },
        }
    }

    /// True when the source is remote provenance (anything but a local path).
    pub fn is_remote(&self) -> bool {
        !matches!(self, SkillSourceRecord::Local { .. })
    }

    /// Short human label for the source kind.
    pub fn kind_label(&self) -> &'static str {
        match self {
            SkillSourceRecord::ClawHub { .. } => "clawhub",
            SkillSourceRecord::Git { .. } => "git",
            SkillSourceRecord::Registry { .. } => "registry",
            SkillSourceRecord::Local { .. } => "local",
        }
    }
}

/// Strip credentials and query/fragment from a git URL for storage. Parseable
/// URLs go through the shared [`super::sanitized_display_url`] sanitizer (single
/// source of truth for URL redaction); SCP-like remotes (`git@host:path`) have
/// any `user:pass@` userinfo removed by hand.
fn sanitize_git_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        return super::sanitized_display_url(&parsed);
    }
    // SCP-like: [user[:pass]@]host:path — drop any password in the userinfo.
    if let Some((userinfo, rest)) = url.split_once('@')
        && !userinfo.contains('/')
        && let Some((user, _pass)) = userinfo.split_once(':')
    {
        return format!("{user}@{rest}");
    }
    url.to_string()
}

/// A persisted install receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInstallReceipt {
    pub schema_version: u32,
    pub name: String,
    /// Typed, sanitized source — never the raw spec string [R5].
    pub source: SkillSourceRecord,
    /// Git commit SHA / zip sha256 digest of the fetched artifact, if known.
    pub immutable_resolution: Option<String>,
    /// Content tree hash (scheme `tree_hash_scheme`).
    pub tree_hash: String,
    pub tree_hash_scheme: u32,
    pub version: Option<String>,
    /// Trust tier at install time — a historical fact; enforcement re-resolves
    /// the live tier.
    pub tier_at_install: String,
    pub screening_ruleset_version: u32,
    pub screening_max_impact: Option<String>,
    pub screening_counts: BTreeMap<String, usize>,
    pub unscanned_count: usize,
    /// Normalized audit options, e.g. `"allow_scripts=false"`.
    pub audit_options: String,
    pub installer_version: String,
    /// Unix seconds at install time.
    pub installed_at: u64,
    /// Content-bound override that was used, if any (I11).
    pub accepted_hash: Option<String>,
}

impl SkillInstallReceipt {
    /// Fold a screening report's summary fields into a partially-built receipt.
    pub fn with_screening(mut self, report: Option<&ScreeningReport>) -> Self {
        if let Some(report) = report {
            self.screening_ruleset_version = report.ruleset_version;
            self.screening_max_impact =
                report.max_impact().map(|i| format!("{i:?}").to_lowercase());
            self.screening_counts = report.impact_counts();
            self.unscanned_count = report.unscanned.len();
        }
        self
    }
}

/// The daemon-owned receipts directory, outside the agent-writable workspace
/// (I4): `<install_root>/state/skill-receipts/`.
pub fn receipts_dir(install_root: &Path) -> PathBuf {
    install_root.join("state").join("skill-receipts")
}

/// Path of a named skill's receipt.
pub fn receipt_path(install_root: &Path, name: &str) -> PathBuf {
    receipts_dir(install_root).join(format!("{name}.json"))
}

/// Persist a receipt. A write failure is the caller's to treat as a warning,
/// not a rollback (the skill is already installed).
pub fn write_receipt(install_root: &Path, receipt: &SkillInstallReceipt) -> Result<()> {
    let dir = receipts_dir(install_root);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create receipts dir {}", dir.display()))?;
    let path = receipt_path(install_root, &receipt.name);
    let json = serde_json::to_string_pretty(receipt).context("failed to serialize receipt")?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write receipt {}", path.display()))?;
    Ok(())
}

/// Read a named skill's receipt, if present and parseable.
pub fn read_receipt(install_root: &Path, name: &str) -> Option<SkillInstallReceipt> {
    let path = receipt_path(install_root, name);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Result of verifying an installed skill against its receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyStatus {
    /// The tree hash matches the receipt.
    Ok,
    /// The tree hash differs — local edit or upstream swap.
    Modified,
    /// No receipt exists (pre-provenance install).
    NoReceipt,
}

/// Verify an installed skill directory against its stored receipt.
pub fn verify_skill(install_root: &Path, name: &str, skill_dir: &Path) -> Result<VerifyStatus> {
    let Some(receipt) = read_receipt(install_root, name) else {
        return Ok(VerifyStatus::NoReceipt);
    };
    let current = compute_tree_hash(skill_dir)?;
    if current == receipt.tree_hash {
        Ok(VerifyStatus::Ok)
    } else {
        Ok(VerifyStatus::Modified)
    }
}

/// Compute the content tree hash of a skill directory (scheme v1).
///
/// Files only, sorted by relative path. For each file, the outer SHA-256 is fed
/// a length-prefixed record:
///   `u64_le(len(rel_bytes)) ‖ rel_bytes ‖ u8(is_executable) ‖
///    u64_le(content_len) ‖ sha256(content)`
/// and the final digest is hex-encoded. Length prefixes make the encoding
/// unambiguous (no path/content can be confused for another) [R5].
///
/// Fails closed on any symlink in the tree: the structural audit rejects
/// symlinks, so one present here was injected after the audit and must not be
/// hashed as if absent.
pub fn compute_tree_hash(dir: &Path) -> Result<String> {
    // Only the per-file `(rel, is_executable, content_len, inner_sha256)` is
    // retained — the inner digest is computed as each file is read, so peak
    // memory is one file rather than the whole tree. The bytes fed to the outer
    // hash are identical to hashing full contents at the end.
    let mut records: Vec<(String, bool, u64, [u8; 32])> = Vec::new();
    for path in collect_paths_depth_first(dir)? {
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!(
                "skill tree contains a symlink, which is not permitted: {}",
                path.display()
            );
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = rel_path_string(dir, &path);
        let content =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let inner: [u8; 32] = Sha256::digest(&content).into();
        records.push((rel, is_executable(&metadata), content.len() as u64, inner));
    }
    // Deterministic order independent of directory-walk order.
    records.sort_by(|a, b| a.0.cmp(&b.0));

    let mut outer = Sha256::new();
    for (rel, exec, content_len, inner) in &records {
        let rel_bytes = rel.as_bytes();
        outer.update((rel_bytes.len() as u64).to_le_bytes());
        outer.update(rel_bytes);
        outer.update([u8::from(*exec)]);
        outer.update(content_len.to_le_bytes());
        outer.update(inner);
    }
    Ok(hex::encode(outer.finalize()))
}

/// Relative path as a `/`-separated string, for stable cross-platform hashing.
fn rel_path_string(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Capture the HEAD commit SHA of a freshly cloned skill, for the receipt's
/// immutable resolution. Best-effort: returns `None` on any error.
pub fn git_head_sha(repo_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

/// sha256 hex digest of a downloaded artifact (e.g. a ClawHub zip), for the
/// receipt's immutable resolution.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn skill_tree(root: &Path) -> PathBuf {
        let dir = root.join("skill");
        write(&dir, "SKILL.md", b"# Skill\nHello.\n");
        write(&dir, "refs/guide.md", b"# Guide\n");
        dir
    }

    #[test]
    fn tree_hash_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let dir = skill_tree(tmp.path());
        assert_eq!(
            compute_tree_hash(&dir).unwrap(),
            compute_tree_hash(&dir).unwrap()
        );
    }

    #[test]
    fn tree_hash_changes_on_flipped_byte() {
        let tmp = TempDir::new().unwrap();
        let dir = skill_tree(tmp.path());
        let before = compute_tree_hash(&dir).unwrap();
        write(&dir, "SKILL.md", b"# Skill\nHellp.\n");
        assert_ne!(before, compute_tree_hash(&dir).unwrap());
    }

    #[test]
    fn tree_hash_changes_on_renamed_file() {
        let tmp = TempDir::new().unwrap();
        let dir = skill_tree(tmp.path());
        let before = compute_tree_hash(&dir).unwrap();
        fs::rename(dir.join("refs/guide.md"), dir.join("refs/manual.md")).unwrap();
        assert_ne!(before, compute_tree_hash(&dir).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn tree_hash_changes_on_exec_bit_flip() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let dir = skill_tree(tmp.path());
        let before = compute_tree_hash(&dir).unwrap();
        let f = dir.join("SKILL.md");
        let mut perms = fs::metadata(&f).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&f, perms).unwrap();
        assert_ne!(before, compute_tree_hash(&dir).unwrap());
    }

    #[test]
    fn length_prefix_prevents_boundary_collision() {
        // Two trees that would collide under naive concatenation must differ:
        // {"ab" -> "c"} vs {"a" -> "bc"} (path/content boundary ambiguity).
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("a");
        write(&a, "ab", b"c");
        let b = tmp.path().join("b");
        write(&b, "a", b"bc");
        assert_ne!(
            compute_tree_hash(&a).unwrap(),
            compute_tree_hash(&b).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn tree_hash_fails_closed_on_symlink() {
        let tmp = TempDir::new().unwrap();
        let dir = skill_tree(tmp.path());
        std::os::unix::fs::symlink(tmp.path().join("SKILL.md"), dir.join("link.md")).unwrap();
        assert!(compute_tree_hash(&dir).is_err());
    }

    #[test]
    fn sanitize_git_url_strips_credentials() {
        assert_eq!(
            sanitize_git_url("https://user:tok@github.com/a/b.git?x=1#f"),
            "https://github.com/a/b.git"
        );
        assert_eq!(
            sanitize_git_url("git@github.com:a/b.git"),
            "git@github.com:a/b.git"
        );
        // A parseable URL has its whole userinfo stripped (the username of an
        // https:// URL can itself be a token), so no credential survives.
        assert_eq!(
            sanitize_git_url("ssh://deploy:secret@host/a/b.git"),
            "ssh://host/a/b.git"
        );
        assert!(!sanitize_git_url("ssh://deploy:secret@host/a/b.git").contains("secret"));
    }

    #[test]
    fn source_record_round_trips_and_sanitizes() {
        let rec = SkillSourceRecord::from_source(&SkillSource::Git {
            url: "https://user:pass@github.com/a/b".to_string(),
        });
        match &rec {
            SkillSourceRecord::Git { url } => assert!(!url.contains("pass")),
            _ => panic!("expected Git"),
        }
        assert!(rec.is_remote());
        let json = serde_json::to_string(&rec).unwrap();
        let back: SkillSourceRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn receipt_write_read_round_trip_and_verify_states() {
        let tmp = TempDir::new().unwrap();
        let install_root = tmp.path().join("home");
        let dir = skill_tree(tmp.path());
        let tree_hash = compute_tree_hash(&dir).unwrap();

        let receipt = SkillInstallReceipt {
            schema_version: RECEIPT_SCHEMA_VERSION,
            name: "skill".to_string(),
            source: SkillSourceRecord::Local {
                path: "/x/skill".to_string(),
            },
            immutable_resolution: None,
            tree_hash: tree_hash.clone(),
            tree_hash_scheme: TREE_HASH_SCHEME,
            version: Some("0.1.0".to_string()),
            tier_at_install: "unknown".to_string(),
            screening_ruleset_version: 1,
            screening_max_impact: None,
            screening_counts: BTreeMap::new(),
            unscanned_count: 0,
            audit_options: "allow_scripts=false".to_string(),
            installer_version: "test".to_string(),
            installed_at: 0,
            accepted_hash: None,
        };

        // No receipt yet.
        assert_eq!(
            verify_skill(&install_root, "skill", &dir).unwrap(),
            VerifyStatus::NoReceipt
        );

        write_receipt(&install_root, &receipt).unwrap();
        let back = read_receipt(&install_root, "skill").unwrap();
        assert_eq!(back.tree_hash, tree_hash);

        // Matches after write.
        assert_eq!(
            verify_skill(&install_root, "skill", &dir).unwrap(),
            VerifyStatus::Ok
        );

        // Modify → Modified.
        write(&dir, "SKILL.md", b"# Skill\nEdited.\n");
        assert_eq!(
            verify_skill(&install_root, "skill", &dir).unwrap(),
            VerifyStatus::Modified
        );
    }
}
