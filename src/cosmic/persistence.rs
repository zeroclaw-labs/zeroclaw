use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosmicSnapshot {
    pub modules: HashMap<String, serde_json::Value>,
    pub version: u32,
    pub saved_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("io: {0}")]
    Io(String),
    #[error("serialization: {0}")]
    Serialization(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("corruption: {0}")]
    Corruption(String),
}

#[derive(Debug, Clone)]
pub struct CosmicPersistence {
    base_dir: PathBuf,
}

impl CosmicPersistence {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    pub fn save_module(
        &self,
        name: &str,
        data: &serde_json::Value,
    ) -> Result<(), PersistenceError> {
        std::fs::create_dir_all(&self.base_dir).map_err(|e| PersistenceError::Io(e.to_string()))?;
        let path = self.base_dir.join(format!("{name}.json"));
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| PersistenceError::Serialization(e.to_string()))?;
        std::fs::write(&path, json).map_err(|e| PersistenceError::Io(e.to_string()))
    }

    pub fn load_module(&self, name: &str) -> Result<serde_json::Value, PersistenceError> {
        let path = self.base_dir.join(format!("{name}.json"));
        if !path.exists() {
            return Err(PersistenceError::NotFound(format!("{name}.json")));
        }
        let raw =
            std::fs::read_to_string(&path).map_err(|e| PersistenceError::Io(e.to_string()))?;
        serde_json::from_str(&raw).map_err(|e| PersistenceError::Corruption(e.to_string()))
    }

    pub fn save_all(&self, snapshot: &CosmicSnapshot) -> Result<(), PersistenceError> {
        std::fs::create_dir_all(&self.base_dir).map_err(|e| PersistenceError::Io(e.to_string()))?;
        for (name, data) in &snapshot.modules {
            self.save_module(name, data)?;
        }
        let meta = serde_json::json!({
            "version": snapshot.version,
            "saved_at": snapshot.saved_at,
            "module_names": snapshot.modules.keys().collect::<Vec<_>>(),
        });
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| PersistenceError::Serialization(e.to_string()))?;
        std::fs::write(self.base_dir.join("_snapshot_meta.json"), meta_json)
            .map_err(|e| PersistenceError::Io(e.to_string()))
    }

    pub fn load_all(&self) -> Result<CosmicSnapshot, PersistenceError> {
        let meta_path = self.base_dir.join("_snapshot_meta.json");
        if !meta_path.exists() {
            return Err(PersistenceError::NotFound(
                "_snapshot_meta.json".to_string(),
            ));
        }
        let meta_raw =
            std::fs::read_to_string(&meta_path).map_err(|e| PersistenceError::Io(e.to_string()))?;
        let meta: serde_json::Value = serde_json::from_str(&meta_raw)
            .map_err(|e| PersistenceError::Corruption(e.to_string()))?;

        #[allow(clippy::cast_possible_truncation)]
        let version = meta["version"].as_u64().unwrap_or(0) as u32;
        let saved_at: DateTime<Utc> = meta["saved_at"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(Utc::now);

        let module_names: Vec<String> = meta["module_names"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let mut modules = HashMap::new();
        for name in &module_names {
            let data = self.load_module(name)?;
            modules.insert(name.clone(), data);
        }

        Ok(CosmicSnapshot {
            modules,
            version,
            saved_at,
        })
    }

    pub fn list_modules(&self) -> Result<Vec<String>, PersistenceError> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }
        let entries =
            std::fs::read_dir(&self.base_dir).map_err(|e| PersistenceError::Io(e.to_string()))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| PersistenceError::Io(e.to_string()))?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.ends_with(".json") && !name.starts_with('_') {
                names.push(name.trim_end_matches(".json").to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn delete_module(&self, name: &str) -> Result<(), PersistenceError> {
        let path = self.base_dir.join(format!("{name}.json"));
        if !path.exists() {
            return Err(PersistenceError::NotFound(format!("{name}.json")));
        }
        std::fs::remove_file(&path).map_err(|e| PersistenceError::Io(e.to_string()))
    }
}

pub fn gather_snapshot(
    modulator: &crate::cosmic::EmotionalModulator,
    drift: &crate::cosmic::DriftDetector,
    thalamus: &crate::cosmic::SensoryThalamus,
    workspace: &crate::cosmic::GlobalWorkspace,
) -> CosmicSnapshot {
    let mut modules = HashMap::new();

    let mod_snap = modulator.snapshot();
    if let Ok(val) = serde_json::to_value(&mod_snap) {
        modules.insert("modulation".to_string(), val);
    }

    let drift_report = drift.drift_report();
    if let Ok(val) = serde_json::to_value(&drift_report) {
        modules.insert("drift".to_string(), val);
    }

    let thal_snap = thalamus.snapshot();
    if let Ok(val) = serde_json::to_value(&thal_snap) {
        modules.insert("thalamus".to_string(), val);
    }

    let ws_snap = workspace.snapshot();
    if let Ok(val) = serde_json::to_value(&ws_snap) {
        modules.insert("workspace".to_string(), val);
    }

    CosmicSnapshot {
        modules,
        version: 1,
        saved_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    #[test]
    fn round_trip_single_module() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        let data = serde_json::json!({"key": "value", "count": 42});
        p.save_module("test_mod", &data).unwrap();
        let loaded = p.load_module("test_mod").unwrap();
        assert_eq!(data, loaded);
    }

    #[test]
    fn round_trip_snapshot() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        let mut modules = HashMap::new();
        modules.insert("alpha".to_string(), serde_json::json!({"a": 1}));
        modules.insert("beta".to_string(), serde_json::json!({"b": 2}));
        let snapshot = CosmicSnapshot {
            modules,
            version: 3,
            saved_at: Utc::now(),
        };
        p.save_all(&snapshot).unwrap();
        let loaded = p.load_all().unwrap();
        assert_eq!(loaded.version, 3);
        assert_eq!(loaded.modules.len(), 2);
        assert_eq!(loaded.modules["alpha"], serde_json::json!({"a": 1}));
        assert_eq!(loaded.modules["beta"], serde_json::json!({"b": 2}));
    }

    #[test]
    fn load_missing_returns_not_found() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        let result = p.load_module("nonexistent");
        assert!(matches!(result, Err(PersistenceError::NotFound(_))));
    }

    #[test]
    fn list_modules_returns_sorted() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        p.save_module("zebra", &serde_json::json!(1)).unwrap();
        p.save_module("alpha", &serde_json::json!(2)).unwrap();
        p.save_module("middle", &serde_json::json!(3)).unwrap();
        let names = p.list_modules().unwrap();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn delete_module_removes_file() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        p.save_module("doomed", &serde_json::json!(1)).unwrap();
        assert!(p.load_module("doomed").is_ok());
        p.delete_module("doomed").unwrap();
        assert!(matches!(
            p.load_module("doomed"),
            Err(PersistenceError::NotFound(_))
        ));
    }

    #[test]
    fn corruption_recovery() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("corrupt.json"), "not valid json {{{").unwrap();
        let result = p.load_module("corrupt");
        assert!(matches!(result, Err(PersistenceError::Corruption(_))));
    }

    #[test]
    fn empty_dir_lists_nothing() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        let names = p.list_modules().unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn save_creates_dir_if_missing() {
        let dir = test_dir();
        let nested = dir.path().join("deep").join("nested");
        let p = CosmicPersistence::new(&nested);
        p.save_module("auto", &serde_json::json!({"created": true}))
            .unwrap();
        let loaded = p.load_module("auto").unwrap();
        assert_eq!(loaded, serde_json::json!({"created": true}));
    }

    #[test]
    fn delete_missing_returns_not_found() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        let result = p.delete_module("ghost");
        assert!(matches!(result, Err(PersistenceError::NotFound(_))));
    }

    #[test]
    fn list_excludes_meta_files() {
        let dir = test_dir();
        let p = CosmicPersistence::new(dir.path());
        let mut modules = HashMap::new();
        modules.insert("only_mod".to_string(), serde_json::json!(1));
        let snapshot = CosmicSnapshot {
            modules,
            version: 1,
            saved_at: Utc::now(),
        };
        p.save_all(&snapshot).unwrap();
        let names = p.list_modules().unwrap();
        assert_eq!(names, vec!["only_mod"]);
    }

    #[test]
    fn gather_snapshot_collects_modules() {
        use crate::cosmic::{DriftDetector, EmotionalModulator, GlobalWorkspace, SensoryThalamus};
        let m = EmotionalModulator::new();
        let d = DriftDetector::new(50, 0.1);
        let t = SensoryThalamus::new(0.3, 100);
        let w = GlobalWorkspace::new(0.3, 5, 50);
        let snap = super::gather_snapshot(&m, &d, &t, &w);
        assert!(snap.modules.contains_key("modulation"));
        assert!(snap.modules.contains_key("drift"));
        assert!(snap.modules.contains_key("thalamus"));
        assert!(snap.modules.contains_key("workspace"));
        assert_eq!(snap.version, 1);
    }
}
