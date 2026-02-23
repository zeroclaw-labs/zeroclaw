//! EVM JSON-RPC provider for on-chain balance queries and transaction sending.

use alloy_primitives::{Address, Bytes, TxHash, U256};
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::eth::{TransactionInput, TransactionReceipt, TransactionRequest};

use super::keypair::WalletKeypair;

pub struct EvmProvider {
    rpc_url: reqwest::Url,
    chain_id: u64,
}

impl EvmProvider {
    pub fn connect(rpc_url: &str, chain_id: u64) -> anyhow::Result<Self> {
        let parsed: reqwest::Url = rpc_url
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid RPC URL: {e}"))?;
        Ok(Self {
            rpc_url: parsed,
            chain_id,
        })
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn read_provider(
        &self,
    ) -> impl Provider {
        ProviderBuilder::new().connect_http(self.rpc_url.clone())
    }

    pub async fn get_balance(&self, address: Address) -> anyhow::Result<U256> {
        let provider = self.read_provider();
        let balance = provider
            .get_balance(address)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get balance: {e}"))?;
        Ok(balance)
    }

    pub async fn send_eth(
        &self,
        keypair: &WalletKeypair,
        to: Address,
        value: U256,
    ) -> anyhow::Result<TxHash> {
        let signer_provider = ProviderBuilder::new()
            .wallet(keypair.signer().clone())
            .connect_http(self.rpc_url.clone());

        let tx = TransactionRequest::default().to(to).value(value);

        let pending = signer_provider
            .send_transaction(tx)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send transaction: {e}"))?;

        Ok(*pending.tx_hash())
    }

    pub async fn get_tx_receipt(
        &self,
        hash: TxHash,
    ) -> anyhow::Result<Option<TransactionReceipt>> {
        let provider = self.read_provider();
        let receipt = provider
            .get_transaction_receipt(hash)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get transaction receipt: {e}"))?;
        Ok(receipt)
    }

    pub async fn call(&self, to: Address, data: Bytes) -> anyhow::Result<Bytes> {
        let provider = self.read_provider();
        let tx = TransactionRequest::default()
            .to(to)
            .input(TransactionInput::new(data));
        let result = provider
            .call(tx)
            .await
            .map_err(|e| anyhow::anyhow!("eth_call failed: {e}"))?;
        Ok(result)
    }

    pub async fn send_contract_tx(
        &self,
        keypair: &WalletKeypair,
        to: Address,
        data: Bytes,
    ) -> anyhow::Result<TxHash> {
        let signer_provider = ProviderBuilder::new()
            .wallet(keypair.signer().clone())
            .connect_http(self.rpc_url.clone());
        let tx = TransactionRequest::default()
            .to(to)
            .input(TransactionInput::new(data));
        let pending = signer_provider
            .send_transaction(tx)
            .await
            .map_err(|e| anyhow::anyhow!("Contract tx failed: {e}"))?;
        Ok(*pending.tx_hash())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_rejects_invalid_url() {
        assert!(EvmProvider::connect("not a url", 1).is_err());
    }

    #[test]
    fn connect_accepts_valid_url() {
        let provider = EvmProvider::connect("https://rpc.sepolia.org", 11155111);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().chain_id(), 11155111);
    }

    #[tokio::test]
    #[ignore]
    async fn sepolia_get_balance() {
        let rpc = std::env::var("ZEROCLAW_TEST_RPC_URL")
            .unwrap_or_else(|_| "https://rpc.sepolia.org".to_string());
        let provider = EvmProvider::connect(&rpc, 11155111).unwrap();
        let zero_addr: Address = "0x0000000000000000000000000000000000000000"
            .parse()
            .unwrap();
        let balance = provider.get_balance(zero_addr).await.unwrap();
        assert!(balance >= U256::ZERO);
    }

    #[tokio::test]
    #[ignore]
    async fn sepolia_token_balance() {
        let rpc = std::env::var("ZEROCLAW_TEST_RPC_URL")
            .unwrap_or_else(|_| "https://rpc.sepolia.org".to_string());
        let provider = EvmProvider::connect(&rpc, 11155111).unwrap();
        let token: Address = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238"
            .parse()
            .unwrap();
        let data = crate::wallet::erc20::encode_balance_of(Address::ZERO);
        let result = provider.call(token, data).await.unwrap();
        assert!(result.len() >= 32);
    }

    #[tokio::test]
    #[ignore]
    async fn sepolia_send_and_receipt() {
        let rpc = std::env::var("ZEROCLAW_TEST_RPC_URL")
            .expect("Set ZEROCLAW_TEST_RPC_URL for network tests");
        let key = std::env::var("ZEROCLAW_TEST_FUNDED_KEY")
            .expect("Set ZEROCLAW_TEST_FUNDED_KEY (hex private key with Sepolia ETH)");

        let provider = EvmProvider::connect(&rpc, 11155111).unwrap();
        let keypair = WalletKeypair::from_hex(&key).unwrap();
        let to: Address = "0x0000000000000000000000000000000000000001"
            .parse()
            .unwrap();

        let tx_hash = provider
            .send_eth(&keypair, to, U256::from(1))
            .await
            .unwrap();
        assert_ne!(tx_hash, TxHash::ZERO);

        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        let receipt = provider.get_tx_receipt(tx_hash).await.unwrap();
        assert!(receipt.is_some());
    }
}
