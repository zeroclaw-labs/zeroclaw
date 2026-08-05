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
//!
//! # Dependency note (cargo audit)
//!
//! This module uses the maintained, narrower Solana interface (the modular
//! `solana-*` crates at their current major versions). It intentionally does
//! NOT pull `solana-keypair`, whose 2.x line pins `ed25519-dalek 1.0.1`
//! (RUSTSEC-2022-0093) and `curve25519-dalek 3.2.0` (RUSTSEC-2024-0344) into
//! the workspace lockfile. Signing is implemented directly on top of
//! `ed25519-dalek 2.x` (the patched line) via the `solana_signer::Signer`
//! trait — the same Ed25519 math, a clean dependency graph.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use solana_address::Address;
use solana_hash::Hash;
use solana_message::legacy::Message;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::{Signer, SignerError};
use solana_system_interface::instruction as system_instruction;
use solana_transaction::Transaction;

// Brings SigningKey::sign into scope (the ed25519-dalek Signer trait).
use ed25519_dalek::Signer as _;

/// An Ed25519 signer backed by `ed25519-dalek 2.x` (patched line).
///
/// Provides the same `Signer` interface `solana-keypair` would, without
/// pulling the audited `ed25519-dalek 1.0.1` dependency tree into the
/// workspace lockfile.
pub struct DalekSigner {
    signing_key: ed25519_dalek::SigningKey,
    verifying_key: ed25519_dalek::VerifyingKey,
}

impl DalekSigner {
    /// Generate a fresh random signer (devnet / tests only — the agent holds
    /// no keys in production; T1 custody).
    pub fn new() -> Self {
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    /// The signer's public key as a Solana `Pubkey`.
    pub fn pubkey(&self) -> Pubkey {
        Pubkey::from(self.verifying_key.to_bytes())
    }
}

impl Default for DalekSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl Signer for DalekSigner {
    fn try_pubkey(&self) -> Result<Pubkey, SignerError> {
        Ok(self.pubkey())
    }

    fn try_sign_message(&self, message: &[u8]) -> Result<Signature, SignerError> {
        let signature = self.signing_key.sign(message);
        Ok(Signature::from(signature.to_bytes()))
    }

    fn is_interactive(&self) -> bool {
        false
    }
}

/// A verified settlement: who gets paid, from which nonce account, how much,
/// and under which authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settlement {
    /// The payee's Solana address (base58).
    pub payee: String,
    /// The durable nonce account that fronts this transaction.
    pub nonce_account: String,
    /// The nonce authority (the mandate holder's wallet).
    pub authority: String,
    /// Amount in lamports to transfer.
    pub lamports: u64,
}

/// Build a durable-nonce transfer transaction.
///
/// The mandate holder is the single signer: they are simultaneously the nonce
/// authority, the fee payer, and the transfer sender. The agent holds no key
/// material (T1 custody), so exactly one signer ever signs a settlement.
pub fn build_durable_nonce_transfer(
    settlement: &Settlement,
    authority: &DalekSigner,
    nonce: &str,
) -> Result<Transaction> {
    let payee_pubkey = Pubkey::from_str(&settlement.payee)
        .map_err(|e| anyhow::Error::msg(format!("payee is not a valid Solana address: {e}")))?;
    let nonce_account = Pubkey::from_str(&settlement.nonce_account).map_err(|e| {
        anyhow::Error::msg(format!("nonce account is not a valid Solana address: {e}"))
    })?;
    let authority_pubkey = Pubkey::from_str(&settlement.authority)
        .map_err(|e| anyhow::Error::msg(format!("authority is not a valid Solana address: {e}")))?;
    if authority_pubkey != authority.pubkey() {
        return Err(anyhow::Error::msg(format!(
            "settlement authority {} does not match signing keypair {}",
            authority_pubkey,
            authority.pubkey()
        )));
    }
    if settlement.lamports == 0 {
        return Err(anyhow::Error::msg("refusing zero-lamport settlement"));
    }
    // A durable nonce transaction is bound to the CURRENT nonce value stored
    // in the nonce account: that value becomes the message's recent_blockhash.
    // A garbage nonce must fail loudly, not silently degrade to Hash::default.
    let nonce_hash = Hash::from_str(nonce)
        .map_err(|e| anyhow::Error::msg(format!("nonce value is not a valid base58 hash: {e}")))?;

    // Addresses for the legacy message / system instructions.
    let payee_addr = Address::from(payee_pubkey.to_bytes());
    let nonce_addr = Address::from(nonce_account.to_bytes());
    let authority_addr = Address::from(authority_pubkey.to_bytes());

    // Instruction 1: advance the nonce. This consumes the current nonce value
    // and derives the next; the transaction is then bound to the *durable*
    // nonce (the nonce_hash above), not to a short-lived recent blockhash.
    let advance = system_instruction::advance_nonce_account(&nonce_addr, &authority_addr);

    // Instruction 2: the transfer itself, from the mandate holder to the payee.
    let transfer = system_instruction::transfer(&authority_addr, &payee_addr, settlement.lamports);

    let message = Message::new(&[advance, transfer], Some(&authority_addr));

    let tx = Transaction::new(&[authority], message, nonce_hash);
    Ok(tx)
}

/// Sign a transaction for devnet submission. The transaction is bound to the
/// durable nonce value (set at build time), so `recent_blockhash` from an RPC
/// is NOT required — signing with the same nonce hash keeps the message
/// consistent. In a real client you would call
/// `solana_client::nonce_utils::get_account` to fetch the nonce, then
/// `transaction::sign` with the nonce authority.
pub fn sign_for_submission(tx: &mut Transaction, signers: &[&DalekSigner]) {
    // The message already carries the durable nonce as recent_blockhash;
    // re-signing with Hash::default would REPLACE it with a dead blockhash and
    // break the nonce binding. Preserve the message's existing blockhash.
    let nonce_hash = tx.message.recent_blockhash;
    tx.sign(signers, nonce_hash);
}

/// Validate that a payee string looks like a Solana address (base58, 32-byte
/// pubkey). Cheap pre-flight before any transaction is built.
pub fn validate_payee(payee: &str) -> bool {
    Pubkey::from_str(payee).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";

    fn signer() -> DalekSigner {
        DalekSigner::new()
    }

    fn sample_settlement_for(authority: &DalekSigner) -> Settlement {
        Settlement {
            payee: signer().pubkey().to_string(),
            lamports: 10_000_000, // 0.01 SOL
            nonce_account: signer().pubkey().to_string(),
            authority: authority.pubkey().to_string(),
        }
    }

    #[test]
    fn validates_real_solana_address() {
        // A real-looking base58 address parses.
        let pk = signer().pubkey().to_string();
        assert!(validate_payee(&pk));
        // Garbage does not.
        assert!(!validate_payee("not-an-address"));
        assert!(!validate_payee(""));
        // Wrong length.
        assert!(!validate_payee("abc123"));
    }

    #[test]
    fn builds_durable_nonce_transaction() {
        let authority = signer();
        let settlement = sample_settlement_for(&authority);
        let nonce = "4pMpYS3iEyR3tn8BeqvqxB7QCULegaiUC6puppPaaE8q";

        let tx = build_durable_nonce_transfer(&settlement, &authority, nonce)
            .expect("valid settlement builds");

        // The message must contain both instructions.
        assert_eq!(tx.message.instructions.len(), 2);
        // The advance instruction targets the System Program.
        let system_program = Pubkey::from_str(SYSTEM_PROGRAM_ID).unwrap();
        let advance_program = tx.message.instructions[0].program_id(&tx.message.account_keys);
        assert_eq!(*advance_program, system_program);
        // The transaction must be bound to the durable nonce: the message's
        // recent_blockhash is the nonce value, not a default/random hash.
        let nonce_hash = Hash::from_str(nonce).unwrap();
        assert_eq!(
            tx.message.recent_blockhash, nonce_hash,
            "durable-nonce transaction must be bound to the nonce value"
        );
    }

    #[test]
    fn rejects_zero_lamports() {
        let authority = signer();
        let mut settlement = sample_settlement_for(&authority);
        settlement.lamports = 0;
        let err = build_durable_nonce_transfer(
            &settlement,
            &authority,
            "4pMpYS3iEyR3tn8BeqvqxB7QCULegaiUC6puppPaaE8q",
        )
        .unwrap_err();
        assert!(err.to_string().contains("zero-lamport"));
    }

    #[test]
    fn rejects_invalid_payee() {
        let authority = signer();
        let mut settlement = sample_settlement_for(&authority);
        settlement.payee = "not-an-address".into();
        let err = build_durable_nonce_transfer(
            &settlement,
            &authority,
            "4pMpYS3iEyR3tn8BeqvqxB7QCULegaiUC6puppPaaE8q",
        )
        .unwrap_err();
        assert!(err.to_string().contains("valid Solana address"));
    }

    #[test]
    fn rejects_invalid_nonce_value() {
        let authority = signer();
        let settlement = sample_settlement_for(&authority);
        // A non-base58 nonce must fail loudly: a durable-nonce transaction
        // bound to a garbage blockhash would be rejected by the cluster
        // anyway, but the builder must never silently fall back to a default
        // hash (which would make the transaction a plain blockhash-bound one).
        let err = build_durable_nonce_transfer(&settlement, &authority, "not-a-nonce").unwrap_err();
        assert!(
            err.to_string().contains("valid base58 hash"),
            "garbage nonce must be rejected: {err}"
        );
    }

    #[test]
    fn rejects_authority_keypair_mismatch() {
        let authority = signer();
        let stranger = signer();
        let settlement = sample_settlement_for(&authority);
        let err = build_durable_nonce_transfer(
            &settlement,
            &stranger,
            "4pMpYS3iEyR3tn8BeqvqxB7QCULegaiUC6puppPaaE8q",
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not match signing keypair"));
    }

    #[test]
    fn two_instructions_one_nonce_account() {
        // One nonce per in-flight transaction: the tx has exactly one
        // AdvanceNonceAccount instruction and one transfer.
        let authority = signer();
        let settlement = sample_settlement_for(&authority);
        let tx = build_durable_nonce_transfer(
            &settlement,
            &authority,
            "4pMpYS3iEyR3tn8BeqvqxB7QCULegaiUC6puppPaaE8q",
        )
        .unwrap();

        // The nonce program legitimately reads the recent-blockhashes sysvar
        // to validate the durable nonce value, so the sysvar account IS in
        // the message. What the durable nonce replaces is the *blockhash
        // binding*: the transaction is valid as long as the nonce is not
        // consumed, instead of dying after ~150 blocks. Assert the structure
        // that actually matters: two instructions, one of them the advance.
        assert_eq!(tx.message.instructions.len(), 2);
        let advance_program = tx.message.instructions[0].program_id(&tx.message.account_keys);
        assert_eq!(
            advance_program,
            &Pubkey::from_str("11111111111111111111111111111111").unwrap()
        );
    }

    #[test]
    fn signature_verifies_after_sign() {
        let authority = signer();
        let settlement = sample_settlement_for(&authority);
        let nonce = "4pMpYS3iEyR3tn8BeqvqxB7QCULegaiUC6puppPaaE8q";
        let mut tx = build_durable_nonce_transfer(&settlement, &authority, nonce).unwrap();
        sign_for_submission(&mut tx, &[&authority]);
        assert_eq!(tx.signatures.len(), 1);
        // The canonical check: every signature in the transaction verifies
        // against the message.
        assert!(
            tx.verify().is_ok(),
            "transaction signatures must verify: {:?}",
            tx.verify().err()
        );
        // Signing must NOT replace the durable nonce binding with a default
        // blockhash: the message still carries the nonce value.
        let nonce_hash = Hash::from_str(nonce).unwrap();
        assert_eq!(
            tx.message.recent_blockhash, nonce_hash,
            "signing must preserve the durable-nonce binding"
        );
    }

    #[test]
    fn devnet_only_no_mainnet() {
        // No mainnet RPC constant exists in the production module. The test
        // must strip its own module (whose assertions mention the banned
        // strings) before scanning, otherwise the check is self-referential.
        // Use a path relative to this file: `file!()` expands against the
        // workspace root when the module is compiled behind a feature flag,
        // which doubles the crate path and breaks include_str!.
        let full = include_str!("solana_settlement.rs");
        let src = full.split("#[cfg(test)]").next().unwrap_or(full);
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
