//! Wallet pay tool — x402 payment protocol execution with treasury enforcement.

use super::traits::{Tool, ToolResult};
use crate::config::TreasuryConfig;
use crate::wallet::storage::WalletStore;
use crate::wallet::x402::{TreasuryLimits, X402Client};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Executes x402 payment protocol for a given URL, enforcing treasury limits.
pub struct WalletPayTool {
    store: Arc<WalletStore>,
    treasury_config: TreasuryConfig,
}

impl WalletPayTool {
    pub fn new(store: Arc<WalletStore>, treasury_config: TreasuryConfig) -> Self {
        Self {
            store,
            treasury_config,
        }
    }

    fn treasury_limits(&self) -> TreasuryLimits {
        TreasuryLimits {
            max_payment_cents: self.treasury_config.max_x402_payment_cents,
            allowed_domains: self.treasury_config.x402_allowed_domains.clone(),
            max_daily_spend_cents: self.treasury_config.max_daily_spend_cents,
            max_monthly_spend_cents: self.treasury_config.max_monthly_spend_cents,
            // Spend tracking would be wired to CostTracker in production;
            // for now, start at 0 per session.
            daily_spent_cents: 0,
            monthly_spent_cents: 0,
        }
    }
}

#[async_trait]
impl Tool for WalletPayTool {
    fn name(&self) -> &str {
        "wallet_pay"
    }

    fn description(&self) -> &str {
        "Pays for access to a URL using the x402 payment protocol. \
         Performs HEAD→402→sign→retry flow. Treasury limits are enforced. \
         Parameters: url (required)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to pay for and fetch"
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: url"))?;

        // Load wallet
        let keypair = self
            .store
            .load()
            .map_err(|e| anyhow::anyhow!("Failed to load wallet: {e}"))?;

        let http_client = reqwest::Client::new();
        let limits = self.treasury_limits();

        let result = X402Client::pay_and_fetch(&http_client, url, &keypair, &limits).await?;

        let output = serde_json::to_string_pretty(&json!({
            "success": result.success,
            "status_code": result.status_code,
            "payment_amount": result.payment_amount,
            "recipient": result.recipient,
            "error": result.error,
        }))?;

        Ok(ToolResult {
            success: result.success,
            output,
            error: result.error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::WalletKeypair;

    fn test_treasury() -> TreasuryConfig {
        TreasuryConfig {
            max_x402_payment_cents: 100,
            x402_allowed_domains: vec![],
            max_daily_spend_cents: 500,
            max_monthly_spend_cents: 5000,
        }
    }

    #[test]
    fn tool_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let tool = WalletPayTool::new(store, test_treasury());
        assert_eq!(tool.name(), "wallet_pay");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "url");
    }

    #[test]
    fn treasury_limits_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let config = TreasuryConfig {
            max_x402_payment_cents: 200,
            x402_allowed_domains: vec!["example.com".into()],
            max_daily_spend_cents: 1000,
            max_monthly_spend_cents: 10000,
        };
        let tool = WalletPayTool::new(store, config);
        let limits = tool.treasury_limits();
        assert_eq!(limits.max_payment_cents, 200);
        assert_eq!(limits.allowed_domains, vec!["example.com"]);
        assert_eq!(limits.max_daily_spend_cents, 1000);
    }

    #[tokio::test]
    async fn pay_fails_without_wallet() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let tool = WalletPayTool::new(store, test_treasury());

        let result = tool
            .execute(json!({ "url": "https://example.com/paid" }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn pay_missing_url_fails() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let kp = WalletKeypair::generate();
        store.save(&kp).unwrap();

        let tool = WalletPayTool::new(store, test_treasury());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
