use std::sync::Arc;

use daemonclaw_memory::StructuredMemory;

use super::types::UserModel;

const USER_MODEL_KEY: &str = "user_model";
const USER_MODEL_CATEGORY: &str = "user_model";

/// Typed wrapper around `StructuredMemory` for the user model.
///
/// All reads and writes go through SQLite with JSON Merge Patch
/// semantics for atomic partial updates.
pub struct UserModelStore {
    memory: Arc<dyn StructuredMemory>,
}

impl UserModelStore {
    pub fn new(memory: Arc<dyn StructuredMemory>) -> Self {
        Self { memory }
    }

    /// Load the current user model, or return a default if none exists.
    pub async fn load(&self) -> anyhow::Result<UserModel> {
        match self.memory.get_json(USER_MODEL_KEY).await? {
            Some(value) => {
                let model: UserModel = serde_json::from_value(value)?;
                Ok(model)
            }
            None => Ok(UserModel::default()),
        }
    }

    /// Replace the entire user model.
    pub async fn save(&self, model: &UserModel) -> anyhow::Result<()> {
        let value = serde_json::to_value(model)?;
        self.memory
            .store_json(USER_MODEL_KEY, &value, USER_MODEL_CATEGORY)
            .await
    }

    /// Apply a partial update via JSON Merge Patch (RFC 7386).
    ///
    /// Only the fields present in the patch are updated; missing fields
    /// are left unchanged. Returns the new merged model.
    pub async fn patch(&self, patch: &serde_json::Value) -> anyhow::Result<UserModel> {
        let merged = self
            .memory
            .patch_json(USER_MODEL_KEY, patch, USER_MODEL_CATEGORY)
            .await?;
        let model: UserModel = serde_json::from_value(merged)?;
        Ok(model)
    }

    /// Render the current model to USER.md content.
    pub async fn render_user_md(&self) -> anyhow::Result<String> {
        let model = self.load().await?;
        Ok(model.render_user_md())
    }

    /// Write USER.md to the workspace directory.
    pub async fn write_user_md(&self, workspace_dir: &std::path::Path) -> anyhow::Result<()> {
        let content = self.render_user_md().await?;
        let path = workspace_dir.join("USER.md");
        tokio::fs::write(&path, content).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory StructuredMemory for testing.
    struct MockStructuredMemory {
        data: Mutex<HashMap<String, serde_json::Value>>,
    }

    impl MockStructuredMemory {
        fn new() -> Self {
            Self {
                data: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl StructuredMemory for MockStructuredMemory {
        async fn store_json(
            &self,
            key: &str,
            value: &serde_json::Value,
            _category: &str,
        ) -> anyhow::Result<()> {
            self.data
                .lock()
                .unwrap()
                .insert(key.to_string(), value.clone());
            Ok(())
        }

        async fn get_json(&self, key: &str) -> anyhow::Result<Option<serde_json::Value>> {
            Ok(self.data.lock().unwrap().get(key).cloned())
        }

        async fn patch_json(
            &self,
            key: &str,
            patch: &serde_json::Value,
            _category: &str,
        ) -> anyhow::Result<serde_json::Value> {
            let mut data = self.data.lock().unwrap();
            let current = data
                .get(key)
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));
            let merged = json_merge_patch(current, patch.clone());
            data.insert(key.to_string(), merged.clone());
            Ok(merged)
        }
    }

    fn json_merge_patch(
        mut target: serde_json::Value,
        patch: serde_json::Value,
    ) -> serde_json::Value {
        if let serde_json::Value::Object(patch_map) = patch {
            if !target.is_object() {
                target = serde_json::Value::Object(Default::default());
            }
            let target_map = target.as_object_mut().unwrap();
            for (k, v) in patch_map {
                if v.is_null() {
                    target_map.remove(&k);
                } else if v.is_object() {
                    let existing = target_map
                        .remove(&k)
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    target_map.insert(k, json_merge_patch(existing, v));
                } else {
                    target_map.insert(k, v);
                }
            }
            target
        } else {
            patch
        }
    }

    #[tokio::test]
    async fn load_default_when_empty() {
        let mem = Arc::new(MockStructuredMemory::new());
        let store = UserModelStore::new(mem);
        let model = store.load().await.unwrap();
        assert_eq!(model.version, 0);
        assert!(model.expertise_areas.is_empty());
    }

    #[tokio::test]
    async fn save_and_load_roundtrip() {
        let mem = Arc::new(MockStructuredMemory::new());
        let store = UserModelStore::new(mem);

        let model = UserModel {
            version: 1,
            communication_style: CommunicationStyle {
                verbosity: Verbosity::Terse,
                ..Default::default()
            },
            expertise_areas: vec![ExpertiseArea {
                domain: "Rust".into(),
                level: ExpertiseLevel::Expert,
                notes: None,
            }],
            ..Default::default()
        };

        store.save(&model).await.unwrap();
        let loaded = store.load().await.unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.expertise_areas.len(), 1);
        assert_eq!(loaded.communication_style.verbosity, Verbosity::Terse);
    }

    #[tokio::test]
    async fn patch_updates_partially() {
        let mem = Arc::new(MockStructuredMemory::new());
        let store = UserModelStore::new(mem);

        let initial = UserModel {
            version: 1,
            communication_style: CommunicationStyle {
                verbosity: Verbosity::Normal,
                tone: Tone::Professional,
                ..Default::default()
            },
            ..Default::default()
        };
        store.save(&initial).await.unwrap();

        let patch = serde_json::json!({
            "communication_style": {
                "verbosity": "terse"
            },
            "version": 2
        });

        let updated = store.patch(&patch).await.unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.communication_style.verbosity, Verbosity::Terse);
        assert_eq!(updated.communication_style.tone, Tone::Professional);
    }

    #[tokio::test]
    async fn render_user_md() {
        let mem = Arc::new(MockStructuredMemory::new());
        let store = UserModelStore::new(mem);

        let model = UserModel {
            expertise_areas: vec![ExpertiseArea {
                domain: "Python".into(),
                level: ExpertiseLevel::Advanced,
                notes: Some("ML focus".into()),
            }],
            ..Default::default()
        };
        store.save(&model).await.unwrap();

        let md = store.render_user_md().await.unwrap();
        assert!(md.contains("# User Profile"));
        assert!(md.contains("Python"));
        assert!(md.contains("ML focus"));
    }
}
