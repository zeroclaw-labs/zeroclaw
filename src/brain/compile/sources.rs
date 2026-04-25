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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(brain_dir: &Path, rel: &str, body: &str) {
        let path = brain_dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    fn minimal_brain(brain_dir: &Path) {
        write(brain_dir, "soul/mind.yaml", "thesis: test mind\n");
        write(
            brain_dir,
            "soul/voice.yaml",
            "communication:\n  - be terse\n",
        );
        write(brain_dir, "soul/judgment.yaml", "default_bias: act_first\n");
        write(brain_dir, "cortex/agents/swarm.yaml", "agents: {}\n");
    }

    #[test]
    fn load_with_only_required_files_succeeds_optional_default_to_null() {
        let tmp = TempDir::new().unwrap();
        minimal_brain(tmp.path());
        let snap = load(tmp.path()).unwrap();
        assert_eq!(
            snap.soul_mind.get("thesis").unwrap().as_str(),
            Some("test mind")
        );
        assert!(matches!(snap.soul_aesthetic, Value::Null));
        assert!(matches!(snap.agile_framework, Value::Null));
        assert!(matches!(snap.messaging_safety, Value::Null));
        assert!(matches!(snap.skills_index, Value::Null));
        assert_eq!(snap.brain_sha.len(), 64);
    }

    #[test]
    fn load_fails_when_required_file_missing() {
        let tmp = TempDir::new().unwrap();
        // Missing cortex/agents/swarm.yaml
        write(tmp.path(), "soul/mind.yaml", "thesis: x\n");
        write(tmp.path(), "soul/voice.yaml", "communication: []\n");
        write(
            tmp.path(),
            "soul/judgment.yaml",
            "default_bias: act_first\n",
        );
        assert!(load(tmp.path()).is_err());
    }

    #[test]
    fn brain_sha_changes_when_content_changes() {
        let tmp1 = TempDir::new().unwrap();
        minimal_brain(tmp1.path());
        let sha_a = load(tmp1.path()).unwrap().brain_sha;

        let tmp2 = TempDir::new().unwrap();
        minimal_brain(tmp2.path());
        write(tmp2.path(), "soul/mind.yaml", "thesis: different\n");
        let sha_b = load(tmp2.path()).unwrap().brain_sha;

        assert_ne!(sha_a, sha_b);
    }

    #[test]
    fn brain_sha_is_stable_for_identical_content() {
        let tmp1 = TempDir::new().unwrap();
        minimal_brain(tmp1.path());
        let sha_a = load(tmp1.path()).unwrap().brain_sha;

        let tmp2 = TempDir::new().unwrap();
        minimal_brain(tmp2.path());
        let sha_b = load(tmp2.path()).unwrap().brain_sha;

        assert_eq!(sha_a, sha_b);
    }

    #[test]
    fn load_fails_on_invalid_yaml() {
        let tmp = TempDir::new().unwrap();
        minimal_brain(tmp.path());
        write(
            tmp.path(),
            "soul/mind.yaml",
            ":\n  - this is not\n: valid: yaml: at all",
        );
        assert!(load(tmp.path()).is_err());
    }
}
