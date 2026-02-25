use anyhow::Result;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::types::Preference;
use super::NarrativeStore;

pub fn continuity_dir(workspace: &Path, override_dir: Option<&Path>) -> Result<PathBuf> {
    let dir = override_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("continuity"));

    let resolved = if dir.exists() {
        dir.canonicalize()?
    } else {
        let mut base = if workspace.exists() {
            workspace.canonicalize()?
        } else {
            workspace.to_path_buf()
        };
        for component in dir.components() {
            match component {
                std::path::Component::Normal(c) => base.push(c),
                std::path::Component::ParentDir => {
                    base.pop();
                }
                _ => {}
            }
        }
        base
    };

    let workspace_resolved = if workspace.exists() {
        workspace.canonicalize()?
    } else {
        workspace.to_path_buf()
    };

    if !resolved.starts_with(&workspace_resolved) {
        anyhow::bail!(
            "continuity directory {} escapes workspace {}",
            resolved.display(),
            workspace_resolved.display()
        );
    }

    Ok(resolved)
}

pub fn load_narrative(dir: &Path, max_episodes: usize) -> Result<NarrativeStore> {
    let path = dir.join("narrative.json");
    if path.exists() {
        let data = std::fs::read_to_string(&path)?;
        let mut store: NarrativeStore = serde_json::from_str(&data)?;
        if store.max_episodes() == 0 {
            store.set_max_episodes(max_episodes);
        }
        Ok(store)
    } else {
        Ok(NarrativeStore::new(max_episodes))
    }
}

pub fn save_narrative(dir: &Path, store: &NarrativeStore) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let data = serde_json::to_string_pretty(store)?;
    atomic_write(&dir.join("narrative.json"), data.as_bytes())?;
    Ok(())
}

pub fn load_preferences(dir: &Path) -> Result<Vec<Preference>> {
    let path = dir.join("preferences.json");
    if path.exists() {
        let data = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&data)?)
    } else {
        Ok(Vec::new())
    }
}

pub fn save_preferences(dir: &Path, prefs: &[Preference]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let data = serde_json::to_string_pretty(prefs)?;
    atomic_write(&dir.join("preferences.json"), data.as_bytes())?;
    Ok(())
}

pub fn save_ledger(dir: &Path, ledger: &crate::conscience::IntegrityLedger) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let data = serde_json::to_string_pretty(ledger)?;
    atomic_write(&dir.join("conscience_ledger.json"), data.as_bytes())?;
    Ok(())
}

pub fn load_ledger(dir: &Path) -> Result<crate::conscience::IntegrityLedger> {
    let path = dir.join("conscience_ledger.json");
    if path.exists() {
        let data = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&data)?)
    } else {
        Ok(crate::conscience::IntegrityLedger::new())
    }
}

pub fn save_evolution_log(dir: &Path, deltas: &[super::types::PreferenceDelta]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("evolution.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for delta in deltas {
        let line = serde_json::to_string(delta)?;
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

pub fn load_evolution_log(dir: &Path) -> Result<Vec<super::types::PreferenceDelta>> {
    let path = dir.join("evolution.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let mut deltas = Vec::new();
    for line in content.lines() {
        if !line.trim().is_empty() {
            deltas.push(serde_json::from_str(line)?);
        }
    }
    Ok(deltas)
}

pub fn save_pruning_archive(dir: &Path, pruned: &[super::types::Preference]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("pruned.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for p in pruned {
        let line = serde_json::to_string(p)?;
        writeln!(file, "{}", line)?;
    }
    Ok(())
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::types::{Episode, PreferenceCategory};

    #[test]
    fn round_trip_narrative() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = NarrativeStore::new(100);
        store.append(Episode {
            summary: "test episode".into(),
            timestamp: 1000,
            significance: 0.5,
            verified: false,
            tags: vec!["test".into()],
            emotional_tag: None,
            valence_score: None,
        });
        save_narrative(dir.path(), &store).unwrap();
        let loaded = load_narrative(dir.path(), 100).unwrap();
        assert_eq!(loaded.episodes().len(), 1);
        assert_eq!(loaded.episodes()[0].summary, "test episode");
    }

    #[test]
    fn round_trip_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = vec![Preference {
            key: "theme".into(),
            value: "dark".into(),
            confidence: 0.9,
            category: PreferenceCategory::Aesthetic,
            last_updated: 1000,
            reasoning: None,
            evolution_history: vec![],
        }];
        save_preferences(dir.path(), &prefs).unwrap();
        let loaded = load_preferences(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].key, "theme");
    }

    #[test]
    fn load_nonexistent_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = load_narrative(dir.path(), 50).unwrap();
        assert!(store.episodes().is_empty());
        let prefs = load_preferences(dir.path()).unwrap();
        assert!(prefs.is_empty());
    }

    #[test]
    fn round_trip_ledger() {
        use crate::conscience::{GateVerdict, IntegrityLedger};
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = IntegrityLedger::new();
        ledger.record_violation("bad_tool", 0.5);
        ledger.add_credit("good_tool", 0.1);
        ledger.record_verdict("shell", GateVerdict::Ask, 0.6, Some(true));
        ledger.record_verdict("file_read", GateVerdict::Allow, 0.9, None);
        save_ledger(dir.path(), &ledger).unwrap();
        let loaded = load_ledger(dir.path()).unwrap();
        assert_eq!(loaded.violations.len(), 1);
        assert_eq!(loaded.credits.len(), 1);
        assert_eq!(loaded.audit_trail.len(), 2);
        assert_eq!(loaded.audit_trail[0].tool_name, "shell");
        assert_eq!(loaded.audit_trail[1].verdict, GateVerdict::Allow);
    }

    #[test]
    fn load_ledger_nonexistent_returns_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = load_ledger(dir.path()).unwrap();
        assert_eq!(ledger.integrity_score, 1.0);
        assert!(ledger.violations.is_empty());
        assert!(ledger.audit_trail.is_empty());
    }
}
