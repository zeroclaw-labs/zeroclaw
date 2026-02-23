//! Wallet balance tool — query on-chain ETH balance via JSON-RPC.

use super::traits::{Tool, ToolResult};
use crate::wallet::provider::EvmProvider;
use crate::wallet::storage::WalletStore;
use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct WalletBalanceTool {
    store: Arc<WalletStore>,
    provider: Arc<EvmProvider>,
}

impl WalletBalanceTool {
    pub fn new(store: Arc<WalletStore>, provider: Arc<EvmProvider>) -> Self {
        Self { store, provider }
    }
}

#[async_trait]
impl Tool for WalletBalanceTool {
    fn name(&self) -> &str {
        "wallet_balance"
    }

    fn description(&self) -> &str {
        "Queries the on-chain ETH balance for an address. \
         If no address is provided, uses the agent's wallet address."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "string",
                    "description": "EVM address to query (0x-prefixed). Defaults to agent wallet."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let address: Address = if let Some(addr_str) = args.get("address").and_then(|v| v.as_str())
        {
            addr_str
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid address: {e}"))?
        } else {
            let addr_str = self
                .store
                .address()
                .map_err(|e| anyhow::anyhow!("No wallet available: {e}"))?;
            addr_str
                .as_str()
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid wallet address: {e}"))?
        };

        let balance = self.provider.get_balance(address).await?;

        let balance_eth = format_wei_to_eth(balance);

        let output = serde_json::to_string_pretty(&json!({
            "address": format!("{address:#x}"),
            "balance_wei": balance.to_string(),
            "balance_eth": balance_eth,
            "chain_id": self.provider.chain_id(),
        }))?;

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

fn format_wei_to_eth(wei: U256) -> String {
    let eth_divisor = U256::from(1_000_000_000_000_000_000u64);
    let whole = wei / eth_divisor;
    let remainder = wei % eth_divisor;
    let remainder_str = format!("{remainder:0>18}");
    let trimmed = remainder_str.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{whole}.0")
    } else {
        format!("{whole}.{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::WalletKeypair;

    fn test_store_with_wallet(tmp: &tempfile::TempDir) -> Arc<WalletStore> {
        let store = Arc::new(WalletStore::new(&tmp.path().join("wallet"), tmp.path()));
        let kp = WalletKeypair::generate();
        store.save(&kp).unwrap();
        store
    }

    #[test]
    fn tool_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = test_store_with_wallet(&tmp);
        let provider =
            Arc::new(EvmProvider::connect("https://rpc.sepolia.org", 11155111).unwrap());
        let tool = WalletBalanceTool::new(store, provider);
        assert_eq!(tool.name(), "wallet_balance");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["type"] == "object");
    }

    #[test]
    fn format_wei_to_eth_works() {
        assert_eq!(
            format_wei_to_eth(U256::from(1_000_000_000_000_000_000u64)),
            "1.0"
        );
        assert_eq!(
            format_wei_to_eth(U256::from(1_500_000_000_000_000_000u64)),
            "1.5"
        );
        assert_eq!(format_wei_to_eth(U256::ZERO), "0.0");
        assert_eq!(format_wei_to_eth(U256::from(1u64)), "0.000000000000000001");
    }
}
