//! Agent-callable secrets management tools.
//!
//! Provides three tools for managing encrypted secrets:
//! - `secrets_list` — list stored secret keys (values are never shown)
//! - `secrets_get` — retrieve a decrypted secret by key
//! - `secrets_store` — store or update an encrypted secret

use super::traits::{Tool, ToolResult};
use crate::security::policy::ToolOperation;
use crate::security::SecretStore;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// File-backed secrets vault: a JSON file of `{ key: encrypted_value }` pairs.
#[derive(Debug, Clone)]
struct SecretsVault {
    path: PathBuf,
    store: SecretStore,
}

impl SecretsVault {
    fn new(workspace_dir: &Path, store: SecretStore) -> Self {
        Self {
            path: workspace_dir.join("secrets.json"),
            store,
        }
    }

    fn load(&self) -> BTreeMap<String, String> {
        let Ok(data) = std::fs::read_to_string(&self.path) else {
            return BTreeMap::new();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    fn save(&self, secrets: &BTreeMap<String, String>) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(secrets)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&self.path, json)
    }

    fn list_keys(&self) -> Vec<String> {
        self.load().keys().cloned().collect()
    }

    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let secrets = self.load();
        match secrets.get(key) {
            Some(encrypted) => {
                let plaintext = self.store.decrypt(encrypted).map_err(|e| e.to_string())?;
                Ok(Some(plaintext))
            }
            None => Ok(None),
        }
    }

    fn put(&self, key: &str, plaintext: &str) -> Result<(), String> {
        let mut secrets = self.load();
        let encrypted = self.store.encrypt(plaintext).map_err(|e| e.to_string())?;
        secrets.insert(key.to_string(), encrypted);
        self.save(&secrets).map_err(|e| e.to_string())
    }
}

// ── SecretsListTool ───────────────────────────────────────────────

pub struct SecretsListTool {
    vault: SecretsVault,
}

impl SecretsListTool {
    pub fn new(workspace_dir: &Path, store: SecretStore) -> Self {
        Self {
            vault: SecretsVault::new(workspace_dir, store),
        }
    }
}

#[async_trait]
impl Tool for SecretsListTool {
    fn name(&self) -> &str {
        "secrets_list"
    }

    fn description(&self) -> &str {
        "List all stored secret keys. Values are never shown — only key names are returned."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let keys = self.vault.list_keys();
        if keys.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No secrets stored.".into(),
                error: None,
            });
        }
        let output = format!("Stored secrets ({}):\n{}", keys.len(), keys.join("\n"));
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ── SecretsGetTool ────────────────────────────────────────────────

pub struct SecretsGetTool {
    vault: SecretsVault,
    security: Arc<SecurityPolicy>,
}

impl SecretsGetTool {
    pub fn new(workspace_dir: &Path, store: SecretStore, security: Arc<SecurityPolicy>) -> Self {
        Self {
            vault: SecretsVault::new(workspace_dir, store),
            security,
        }
    }
}

#[async_trait]
impl Tool for SecretsGetTool {
    fn name(&self) -> &str {
        "secrets_get"
    }

    fn description(&self) -> &str {
        "Retrieve a decrypted secret by its key name. Use with care — the plaintext value is returned."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "The secret key name to retrieve" }
            },
            "required": ["key"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "secrets_get")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'key' parameter"))?;

        match self.vault.get(key) {
            Ok(Some(value)) => Ok(ToolResult {
                success: true,
                output: value,
                error: None,
            }),
            Ok(None) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Secret '{key}' not found.")),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to decrypt secret: {e}")),
            }),
        }
    }
}

// ── SecretsStoreTool ──────────────────────────────────────────────

pub struct SecretsStoreTool {
    vault: SecretsVault,
    security: Arc<SecurityPolicy>,
}

impl SecretsStoreTool {
    pub fn new(workspace_dir: &Path, store: SecretStore, security: Arc<SecurityPolicy>) -> Self {
        Self {
            vault: SecretsVault::new(workspace_dir, store),
            security,
        }
    }
}

#[async_trait]
impl Tool for SecretsStoreTool {
    fn name(&self) -> &str {
        "secrets_store"
    }

    fn description(&self) -> &str {
        "Store or update an encrypted secret. The value is encrypted with ChaCha20-Poly1305 before writing to disk."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "The secret key name" },
                "value": { "type": "string", "description": "The plaintext secret value to store" }
            },
            "required": ["key", "value"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "secrets_store")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'key' parameter"))?;

        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'value' parameter"))?;

        if key.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Secret key must not be empty.".into()),
            });
        }

        match self.vault.put(key, value) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Secret '{key}' stored (encrypted)."),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to store secret: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_env() -> (tempfile::TempDir, SecretStore, Arc<SecurityPolicy>) {
        let tmp = tempfile::tempdir().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let security = Arc::new(SecurityPolicy::default());
        (tmp, store, security)
    }

    #[tokio::test]
    async fn list_empty_vault() {
        let (tmp, store, _sec) = test_env();
        let tool = SecretsListTool::new(tmp.path(), store);
        let r = tool.execute(json!({})).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("No secrets"));
    }

    #[tokio::test]
    async fn store_and_list() {
        let (tmp, store, sec) = test_env();
        let store_tool = SecretsStoreTool::new(tmp.path(), store.clone(), sec);
        let r = store_tool
            .execute(json!({"key": "API_KEY", "value": "sk-12345"}))
            .await
            .unwrap();
        assert!(r.success);

        let list_tool = SecretsListTool::new(tmp.path(), store);
        let r = list_tool.execute(json!({})).await.unwrap();
        assert!(r.output.contains("API_KEY"));
        assert!(!r.output.contains("sk-12345")); // value never shown in list
    }

    #[tokio::test]
    async fn store_and_get() {
        let (tmp, store, sec) = test_env();
        let store_tool = SecretsStoreTool::new(tmp.path(), store.clone(), sec.clone());
        store_tool
            .execute(json!({"key": "TOKEN", "value": "my-secret-token"}))
            .await
            .unwrap();

        let get_tool = SecretsGetTool::new(tmp.path(), store, sec);
        let r = get_tool.execute(json!({"key": "TOKEN"})).await.unwrap();
        assert!(r.success);
        assert_eq!(r.output, "my-secret-token");
    }

    #[tokio::test]
    async fn get_nonexistent_key() {
        let (tmp, store, sec) = test_env();
        let tool = SecretsGetTool::new(tmp.path(), store, sec);
        let r = tool.execute(json!({"key": "nope"})).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn store_rejects_empty_key() {
        let (tmp, store, sec) = test_env();
        let tool = SecretsStoreTool::new(tmp.path(), store, sec);
        let r = tool
            .execute(json!({"key": "", "value": "x"}))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn overwrite_existing_secret() {
        let (tmp, store, sec) = test_env();
        let tool = SecretsStoreTool::new(tmp.path(), store.clone(), sec.clone());
        tool.execute(json!({"key": "K", "value": "v1"}))
            .await
            .unwrap();
        tool.execute(json!({"key": "K", "value": "v2"}))
            .await
            .unwrap();

        let get = SecretsGetTool::new(tmp.path(), store, sec);
        let r = get.execute(json!({"key": "K"})).await.unwrap();
        assert_eq!(r.output, "v2");
    }

    #[test]
    fn tool_names_and_schemas() {
        let (tmp, store, sec) = test_env();
        let list = SecretsListTool::new(tmp.path(), store.clone());
        let get = SecretsGetTool::new(tmp.path(), store.clone(), sec.clone());
        let put = SecretsStoreTool::new(tmp.path(), store, sec);
        assert_eq!(list.name(), "secrets_list");
        assert_eq!(get.name(), "secrets_get");
        assert_eq!(put.name(), "secrets_store");
        assert!(get.parameters_schema()["required"]
            .as_array()
            .unwrap()
            .contains(&json!("key")));
        assert!(put.parameters_schema()["required"]
            .as_array()
            .unwrap()
            .contains(&json!("key")));
        assert!(put.parameters_schema()["required"]
            .as_array()
            .unwrap()
            .contains(&json!("value")));
    }
}
