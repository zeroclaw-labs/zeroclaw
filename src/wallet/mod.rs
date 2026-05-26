//! EVM wallet module — key management, signing, and x402 payment protocol.
//!
//! Feature-gated behind `#[cfg(feature = "wallet")]`. Provides:
//! - Private key generation and address derivation (secp256k1)
//! - Encrypted wallet storage via the existing `SecretStore`
//! - EIP-712 typed data signing for x402 payments
//! - x402 payment protocol (HEAD→402→sign→retry)
//! - On-chain balance queries and ETH transfers via JSON-RPC

pub mod erc20;
pub mod keypair;
pub mod provider;
pub mod signing;
pub mod storage;
pub mod x402;

#[allow(unused_imports)]
pub use keypair::{WalletAddress, WalletKeypair};
pub use provider::EvmProvider;
#[allow(unused_imports)]
pub use signing::Eip712Signer;
#[allow(unused_imports)]
pub use storage::WalletStore;
#[allow(unused_imports)]
pub use x402::{X402Client, X402PaymentResult};
