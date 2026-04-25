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
