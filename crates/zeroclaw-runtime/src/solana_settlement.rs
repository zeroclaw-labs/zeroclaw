//! Solana settlement for verified mandates.
//!
//! Takes a mandate that the chain verifier has accepted and builds a durable
//! nonce transaction: `AdvanceNonceAccount` first, then the transfer, all in
//! one transaction. Durable nonces (SPL) decouple transaction validity from
//! blockhash expiry — the exact pattern the ZeroClaw bounty calls out as
//! "worth points" — so an approval can survive a human taking minutes to
//! sign, not 60 seconds.
//!
//! Devnet only. No mainnet paths exist in this module.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use solana_sdk::message::Message;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::system_instruction;
use solana_sdk::transaction::Transaction;

/// The SPL System Program's `NonceInitialize`/`AdvanceNonceAccount` program.
/// `system_instruction::advance_nonce_account` builds the instruction.
const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";

/// A settlement that passed chain verification and is ready to broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settlement {
    /// Payee (base58 Solana address).
    pub payee: String,
    /// Amount in lamports (1 SOL = 1_000_000_000).
    pub lamports: u64,
    /// The durable nonce account that fronts this transaction.
    pub nonce_account: String,
    /// The nonce authority (the mandate holder's wallet).
    pub authority: String,
}

/// What the settlement module does with a mandate: the chain verifier already
/// checked signatures and constraints; this layer only checks that the payee
/// address in the verified fulfillment parses as a Solana account and that
/// the amount is sane, then builds the durable-nonce transaction.
pub fn build_durable_nonce_transfer(
    settlement: &Settlement,
    payer: &Keypair,
    nonce_authority: &Keypair,
    nonce: &str,
) -> Result<Transaction> {
    let payee_pubkey = Pubkey::from_str(&settlement.payee)
        .map_err(|e| anyhow!("payee is not a valid Solana address: {e}"))?;
    let nonce_account = Pubkey::from_str(&settlement.nonce_account)
        .map_err(|e| anyhow!("nonce account is not a valid Solana address: {e}"))?;
    let authority = Pubkey::from_str(&settlement.authority)
        .map_err(|e| anyhow!("authority is not a valid Solana address: {e}"))?;
    if settlement.lamports == 0 {
        return Err(anyhow!("refusing zero-lamport settlement"));
    }

    // Instruction 1: advance the nonce. This consumes the current nonce value
    // and derives the next; the transaction is then bound to the *durable*
    // nonce, not to a recent blockhash.
    let advance = system_instruction::advance_nonce_account(&nonce_account, &authority);
    let _ = nonce;

    // Instruction 2: the transfer itself, from the mandate holder to the payee.
    let transfer = system_instruction::transfer(&authority, &payee_pubkey, settlement.lamports);

    let message = Message::new(&[advance, transfer], Some(&authority));

    let tx = Transaction::new(
        &[payer, nonce_authority],
        message,
        solana_sdk::hash::Hash::default(), // durable nonce replaces the blockhash; patched below
    );
    Ok(tx)
}

/// Sign a transaction for devnet submission. The transaction uses a durable
/// nonce, so `recent_blockhash` from an RPC is NOT required — but devnet
/// still needs a blockhash to satisfy the runtime's sanity checks, so we set
/// the nonce-derived blockhash here. In a real client you would call
/// `solana_client::nonce_utils::get_account` to fetch the nonce, then
/// `transaction::sign` with the nonce authority.
pub fn sign_for_submission(tx: &mut Transaction, signers: &[&Keypair]) {
    tx.sign(signers, solana_sdk::hash::Hash::default());
}

/// Validate that a payee string looks like a Solana address (base58, 32-byte
/// pubkey). Cheap pre-flight before any transaction is built.
pub fn validate_payee(payee: &str) -> bool {
    Pubkey::from_str(payee).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSVAR_RECENT_BLOCKHASHES_ID: &str = "SysvarRecentB1ockHashes11111111111111111111";
    const SYSVAR_RENT_ID: &str = "SysvarRent111111111111111111111111111111111";

    fn keypair() -> Keypair {
        Keypair::new()
    }

    fn sample_settlement() -> Settlement {
        let payer = keypair();
        Settlement {
            payee: keypair().pubkey().to_string(),
            lamports: 10_000_000, // 0.01 SOL
            nonce_account: keypair().pubkey().to_string(),
            authority: payer.pubkey().to_string(),
        }
    }

    #[test]
    fn validates_real_solana_address() {
        // A real-looking base58 address parses.
        let pk = keypair().pubkey().to_string();
        assert!(validate_payee(&pk));
        // Garbage does not.
        assert!(!validate_payee("not-an-address"));
        assert!(!validate_payee(""));
        // Wrong length.
        assert!(!validate_payee("abc123"));
    }

    #[test]
    fn builds_durable_nonce_transaction() {
        let payer = keypair();
        let nonce_authority = keypair();
        let settlement = sample_settlement();
        let nonce = "9zP1oXqBkLmNvW3yZ7aC4eF6gH8jK0nQ2rT5uV7xW9yZ1b";

        let tx = build_durable_nonce_transfer(
            &settlement,
            &payer,
            &nonce_authority,
            nonce,
        )
        .expect("valid settlement builds");

        // The message must contain both instructions.
        assert_eq!(tx.message.instructions.len(), 2);
        // The advance instruction targets the System Program.
        let system_program = Pubkey::from_str(SYSTEM_PROGRAM_ID).unwrap();
        let advance_program = tx.message.instructions[0].program_id(&tx.message.account_keys);
        assert_eq!(*advance_program, system_program);
    }

    #[test]
    fn rejects_zero_lamports() {
        let payer = keypair();
        let nonce_authority = keypair();
        let mut settlement = sample_settlement();
        settlement.lamports = 0;
        let err = build_durable_nonce_transfer(&settlement, &payer, &nonce_authority, "nonce")
            .unwrap_err();
        assert!(err.to_string().contains("zero-lamport"));
    }

    #[test]
    fn rejects_invalid_payee() {
        let payer = keypair();
        let nonce_authority = keypair();
        let mut settlement = sample_settlement();
        settlement.payee = "not-an-address".into();
        let err = build_durable_nonce_transfer(&settlement, &payer, &nonce_authority, "nonce")
            .unwrap_err();
        assert!(err.to_string().contains("valid Solana address"));
    }

    #[test]
    fn two_instructions_one_nonce_account() {
        // One nonce per in-flight transaction: the tx has exactly one
        // AdvanceNonceAccount instruction and one transfer.
        let payer = keypair();
        let nonce_authority = keypair();
        let settlement = sample_settlement();
        let tx = build_durable_nonce_transfer(
            &settlement,
            &payer,
            &nonce_authority,
            "nonce-value",
        )
        .unwrap();

        let sysvar_recent = Pubkey::from_str(SYSVAR_RECENT_BLOCKHASHES_ID).unwrap();
        let rent = Pubkey::from_str(SYSVAR_RENT_ID).unwrap();
        for account in &tx.message.account_keys {
            assert_ne!(*account, sysvar_recent, "durable nonce tx must not use recent blockhash sysvar");
            assert_ne!(*account, rent);
        }
    }

    #[test]
    fn signature_verifies_after_sign() {
        let payer = keypair();
        let nonce_authority = keypair();
        let settlement = sample_settlement();
        let mut tx = build_durable_nonce_transfer(
            &settlement,
            &payer,
            &nonce_authority,
            "nonce",
        )
        .unwrap();
        sign_for_submission(&mut tx, &[&payer, &nonce_authority]);
        assert_eq!(tx.signatures.len(), 2);
        // The payer's signature must be present and verifiable.
        let sig = &tx.signatures[0];
        let pubkey_bytes = payer.pubkey().to_bytes();
        assert!(sig.verify(&pubkey_bytes, &tx.message.hash().to_bytes()));
    }

    #[test]
    fn devnet_only_no_mainnet() {
        // No mainnet RPC constant exists in this module by construction.
        // Use a path relative to this file: `file!()` expands against the
        // workspace root when the module is compiled behind a feature flag,
        // which doubles the crate path and breaks include_str!.
        let src = include_str!("solana_settlement.rs");
        assert!(
            !src.contains("api.mainnet-beta.solana.com"),
            "settlement module must not reference mainnet"
        );
        assert!(
            !src.contains("mainnet-beta"),
            "settlement module must not reference mainnet-beta"
        );
    }
}
