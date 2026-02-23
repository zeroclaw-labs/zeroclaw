//! Wallet info tool — read-only introspection into agent wallet state.

use super::traits::{Tool, ToolResult};
use crate::wallet::storage::WalletStore;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Read-only tool that returns the agent's wallet address and status.
pub struct WalletInfoTool {
    store: Arc<WalletStore>,
}

impl WalletInfoTool {
    pub fn new(store: Arc<WalletStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for WalletInfoTool {
    fn name(&self) -> &str {
        "wallet_info"
    }

    fn description(&self) -> &str {
        "Returns the agent's EVM wallet address and status. Read-only, no signing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let exists = self.store.exists();
        let address = if exists {
            self.store.address().ok()
        } else {
            None
        };

        let output = serde_json::to_string_pretty(&json!({
            "wallet_exists": exists,
            "address": address,
            "wallet_path": self.store.wallet_path().to_string_lossy(),
        }))?;

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::WalletKeypair;

    fn test_store(tmp: &tempfile::TempDir) -> Arc<WalletStore> {
        Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()))
    }

    #[test]
    fn tool_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletInfoTool::new(test_store(&tmp));
        assert_eq!(tool.name(), "wallet_info");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["type"] == "object");
    }

    #[tokio::test]
    async fn no_wallet_returns_not_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = WalletInfoTool::new(test_store(&tmp));
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"wallet_exists\": false"));
    }

    #[tokio::test]
    async fn existing_wallet_returns_address() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = test_store(&tmp);
        let kp = WalletKeypair::generate();
        store.save(&kp).unwrap();

        let tool = WalletInfoTool::new(store);
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"wallet_exists\": true"));
        assert!(result.output.contains("0x"));
    }
}
