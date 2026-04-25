//! Load brain content from `~/.brain/`. Keep raw YAML around for the renderer
//! to walk; we explicitly do NOT splat YAML into output files. The renderer
//! reads structured fields and produces narrative prose.

use anyhow::{Context, Result};
use serde_yaml::Value;
use std::fs;
use std::path::Path;

pub struct BrainSnapshot {
    pub soul_mind: Value,
    pub soul_voice: Value,
    pub soul_judgment: Value,
    pub soul_aesthetic: Value,
    pub swarm: Value,
    pub agile_framework: Value,
    pub messaging_safety: Value,
    pub skills_index: Value,
    /// SHA-256 of all loaded source bytes — used as a provenance stamp.
    pub brain_sha: String,
    pub compiled_at: String,
}

pub fn load(brain_dir: &Path) -> Result<BrainSnapshot> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    let load = |rel: &str, hasher: &mut Sha256| -> Result<Value> {
        let path = brain_dir.join(rel);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(&bytes);
        let v: Value =
            serde_yaml::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        Ok(v)
    };

    let soul_mind = load("soul/mind.yaml", &mut hasher)?;
    let soul_voice = load("soul/voice.yaml", &mut hasher)?;
    let soul_judgment = load("soul/judgment.yaml", &mut hasher)?;
    let soul_aesthetic = load("soul/aesthetic.yaml", &mut hasher).unwrap_or(Value::Null);
    let swarm = load("cortex/agents/swarm.yaml", &mut hasher)?;
    let agile_framework =
        load("governance/agile-framework.yaml", &mut hasher).unwrap_or(Value::Null);
    let messaging_safety =
        load("governance/messaging_safety.yaml", &mut hasher).unwrap_or(Value::Null);
    let skills_index = load("skills/__index.yaml", &mut hasher).unwrap_or(Value::Null);

    let brain_sha = format!("{:x}", hasher.finalize());
    let compiled_at = chrono::Utc::now().to_rfc3339();

    Ok(BrainSnapshot {
        soul_mind,
        soul_voice,
        soul_judgment,
        soul_aesthetic,
        swarm,
        agile_framework,
        messaging_safety,
        skills_index,
        brain_sha,
        compiled_at,
    })
}
