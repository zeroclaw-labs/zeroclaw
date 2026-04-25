//! Atomic write with hash-skip-if-unchanged. Avoids spurious agent-bundle
//! reloads on every 15-min compile loop.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

use super::render::AgentBundle;

pub enum WriteOutcome {
    Wrote,
    Unchanged,
    WouldWrite,
}

pub fn write_bundle(
    dir: &Path,
    bundle: &AgentBundle,
    force: bool,
    dry_run: bool,
) -> Result<WriteOutcome> {
    let entries = [
        ("AGENTS.md", &bundle.agents_md),
        ("SOUL.md", &bundle.soul_md),
        ("TOOLS.md", &bundle.tools_md),
    ];

    if !dry_run {
        fs::create_dir_all(dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
    }

    let mut any_change = false;
    for (name, content) in entries {
        let target = dir.join(name);
        let new_hash = sha256_hex(content.as_bytes());
        let unchanged = match fs::read(&target) {
            Ok(existing) => sha256_hex(&existing) == new_hash,
            Err(_) => false,
        };
        if unchanged && !force {
            continue;
        }
        any_change = true;
        if dry_run {
            continue;
        }
        write_atomic(&target, content)?;
    }

    if !any_change {
        return Ok(WriteOutcome::Unchanged);
    }
    if dry_run {
        return Ok(WriteOutcome::WouldWrite);
    }
    Ok(WriteOutcome::Wrote)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn write_atomic(target: &Path, content: &str) -> Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        target.file_name().and_then(|s| s.to_str()).unwrap_or("out"),
        std::process::id()
    ));
    fs::write(&tmp, content).with_context(|| format!("write tmp {}", tmp.display()))?;
    fs::rename(&tmp, target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn bundle(tag: &str) -> AgentBundle {
        AgentBundle {
            agents_md: format!("agents-{tag}"),
            soul_md: format!("soul-{tag}"),
            tools_md: format!("tools-{tag}"),
        }
    }

    #[test]
    fn first_write_creates_files_and_reports_wrote() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("instructions");
        let outcome = write_bundle(&dir, &bundle("v1"), false, false).unwrap();
        assert!(matches!(outcome, WriteOutcome::Wrote));
        assert_eq!(
            fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
            "agents-v1"
        );
        assert_eq!(fs::read_to_string(dir.join("SOUL.md")).unwrap(), "soul-v1");
        assert_eq!(
            fs::read_to_string(dir.join("TOOLS.md")).unwrap(),
            "tools-v1"
        );
    }

    #[test]
    fn second_write_with_identical_content_reports_unchanged() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("instructions");
        write_bundle(&dir, &bundle("v1"), false, false).unwrap();
        let outcome = write_bundle(&dir, &bundle("v1"), false, false).unwrap();
        assert!(matches!(outcome, WriteOutcome::Unchanged));
    }

    #[test]
    fn dry_run_with_no_existing_files_reports_would_write_and_creates_nothing() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("instructions");
        let outcome = write_bundle(&dir, &bundle("v1"), false, true).unwrap();
        assert!(matches!(outcome, WriteOutcome::WouldWrite));
        assert!(!dir.exists(), "dry-run must not create the target dir");
    }

    #[test]
    fn dry_run_with_unchanged_content_reports_unchanged() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("instructions");
        write_bundle(&dir, &bundle("v1"), false, false).unwrap();
        let outcome = write_bundle(&dir, &bundle("v1"), false, true).unwrap();
        assert!(matches!(outcome, WriteOutcome::Unchanged));
    }

    #[test]
    fn force_rewrites_even_when_content_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("instructions");
        write_bundle(&dir, &bundle("v1"), false, false).unwrap();
        let outcome = write_bundle(&dir, &bundle("v1"), true, false).unwrap();
        assert!(matches!(outcome, WriteOutcome::Wrote));
    }

    #[test]
    fn changed_content_overwrites() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("instructions");
        write_bundle(&dir, &bundle("v1"), false, false).unwrap();
        let outcome = write_bundle(&dir, &bundle("v2"), false, false).unwrap();
        assert!(matches!(outcome, WriteOutcome::Wrote));
        assert_eq!(
            fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
            "agents-v2"
        );
    }
}
