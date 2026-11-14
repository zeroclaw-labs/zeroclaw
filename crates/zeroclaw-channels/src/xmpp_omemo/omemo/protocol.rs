// src/omemo/protocol.rs
//! Implementation of the OMEMO protocol according to XEP-0384
//!
//! This module handles the cryptographic protocol for OMEMO, including X3DH and Double Ratchet.

#[cfg(test)]
use crate::xmpp_omemo::omemo::keys::EphemeralPrivateKey;
use anyhow::Result;
use serde::{Deserialize, Serialize};

use thiserror::Error;

use crate::xmpp_omemo::jid::BareJid;
use crate::xmpp_omemo::omemo::crypto;
use crate::xmpp_omemo::omemo::device_id::DeviceId;
use crate::xmpp_omemo::omemo::keys::{
    AesCbcKey, CbcIv, ChainKey, Ikm, MessageKey, PublicKey, Receiving, RootKey, Salt, Secret,
    Sending,
};

/// Errors that can occur in double ratchet operations
#[derive(Debug, Error)]
pub enum DoubleRatchetError {
    /// Error during cryptographic operations
    #[error("Crypto error: {0}")]
    Crypto(#[from] crate::xmpp_omemo::omemo::crypto::CryptoError),

    /// Invalid signature
    #[error("Invalid signature: {0}")]
    InvalidSignature(String),

    /// Invalid message format
    #[error("Invalid message format: {0}")]
    InvalidMessageFormat(String),
}

/// A key pair for OMEMO operations
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPair {
    /// The public key (32-byte normalized Montgomery form)
    pub public_key: PublicKey,

    /// The private key — zeroized on drop, access via .expose_secret()
    pub private_key: Secret<32>,
}

/// Implements X3DH protocol for OMEMO
pub struct X3DHProtocol;

/// A pre-key bundle format for OMEMO
#[derive(Clone, Serialize, Deserialize)]
pub struct PreKeyBundle {
    /// Pre-key ID
    pub id: u32,

    /// Pre-key public key
    pub public_key: Vec<u8>,
}

/// A signed pre-key bundle format for OMEMO
#[derive(Clone, Serialize, Deserialize)]
pub struct SignedPreKeyBundle {
    /// Signed pre-key ID
    pub id: u32,

    /// Signed pre-key public key
    pub public_key: Vec<u8>,

    /// Signature of the signed pre-key
    pub signature: Vec<u8>,
}

/// A device identity for OMEMO
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// Device ID
    pub id: DeviceId,

    /// Identity key
    pub identity_key: Vec<u8>,

    /// Signed pre-key
    pub signed_pre_key: SignedPreKeyBundle,

    /// Pre-keys
    pub pre_keys: Vec<PreKeyBundle>,
}

impl DeviceIdentity {
    /// Verify the SPK signature and return a proof token.
    /// The only way to obtain `VerifiedDeviceIdentity`.
    pub fn verify(self) -> Result<VerifiedDeviceIdentity, DoubleRatchetError> {
        X3DHProtocol::verify_pre_key(
            &self.identity_key,
            self.signed_pre_key.public_key.as_ref(),
            &self.signed_pre_key.signature,
        )?;
        Ok(VerifiedDeviceIdentity(self))
    }
}

/// A `DeviceIdentity` whose SPK signature has been verified.
/// No public constructor — only produced by `DeviceIdentity::verify()`.
/// Pass this to X3DH session creation so "forgot to verify" is a compile error.
pub struct VerifiedDeviceIdentity(pub(crate) DeviceIdentity);

impl std::ops::Deref for VerifiedDeviceIdentity {
    type Target = DeviceIdentity;
    fn deref(&self) -> &DeviceIdentity {
        &self.0
    }
}

/// State for the Double Ratchet
#[derive(Clone, Serialize, Deserialize)]
pub struct RatchetState {
    /// Flag indicating if the state is initialized
    pub initialized: bool,

    /// Flag indicating if this is the initiator
    pub is_initiator: bool,

    /// Remote identity key
    pub remote_identity_key: Vec<u8>,

    /// Local identity key pair
    pub local_identity_key_pair: KeyPair,

    /// Root key — zeroized on drop
    pub root_key: RootKey,

    /// Send chain key — zeroized on drop
    pub send_chain_key: ChainKey<Sending>,

    /// Receive chain key — zeroized on drop
    pub receive_chain_key: ChainKey<Receiving>,

    /// Ratchet key pair
    pub ratchet_key_pair: KeyPair,

    /// Remote ratchet key
    pub remote_ratchet_key: Vec<u8>,

    /// Previous remote ratchet key
    pub prev_remote_ratchet_key: Vec<u8>,

    /// Send message number
    pub send_message_number: u32,

    /// Receive message number
    pub receive_message_number: u32,

    /// Previous receive message number
    pub prev_receive_message_number: u32,

    /// Previous send message number (used as previous_counter in Signal wire format)
    #[serde(default)]
    pub prev_send_message_number: u32,

    /// Skipped message keys
    pub skipped_message_keys: std::collections::HashMap<(Vec<u8>, u32), MessageKey>,

    /// Local device ID
    pub local_device_id: DeviceId,

    /// Remote device ID
    pub remote_device_id: DeviceId,

    /// Remote JID
    pub remote_jid: String,

    /// For sessions we accepted as the X3DH *recipient*: the initiator's base
    /// (ephemeral) key from the PreKeySignalMessage that established this
    /// session.
    ///
    /// libsignal initiators keep attaching the PreKey header to *every*
    /// outbound message until they receive one back inside the session (the
    /// "unacknowledged prekey" state).  Those retransmits carry the same base
    /// key and the same one-time-prekey id that we already consumed.  Recording
    /// the base key lets `decrypt_message` recognise a retransmit and decrypt
    /// it with the existing session instead of trying — and failing — to re-run
    /// X3DH against a one-time prekey that no longer exists.
    ///
    /// `None` for initiator sessions.
    ///
    /// NOTE: `#[serde(default)]` does **not** make older on-disk states
    /// loadable.  Sessions are persisted with `bincode`, which is not
    /// self-describing — see [`legacy_ratchet`] for how pre-existing
    /// `state.bin` files are decoded and upgraded.
    #[serde(default)]
    pub establishing_base_key: Option<Vec<u8>>,
}

impl std::fmt::Debug for RatchetState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RatchetState")
            .field("initialized", &self.initialized)
            .field("is_initiator", &self.is_initiator)
            .field("remote_jid", &self.remote_jid)
            .field("local_device_id", &self.local_device_id)
            .field("remote_device_id", &self.remote_device_id)
            .field("send_message_number", &self.send_message_number)
            .field("receive_message_number", &self.receive_message_number)
            .field("skipped_keys_count", &self.skipped_message_keys.len())
            .field("root_key", &"Secret<32>(redacted)")
            .field("send_chain_key", &"Secret<32>(redacted)")
            .field("receive_chain_key", &"Secret<32>(redacted)")
            .finish()
    }
}

/// Double Ratchet implementation for OMEMO
pub struct DoubleRatchet;

impl X3DHProtocol {
    /// Generate a key pair for OMEMO operations
    pub fn generate_key_pair() -> Result<KeyPair, DoubleRatchetError> {
        // generate_x25519_keypair returns (private_bytes, public_bytes)
        let (private_key_bytes, public_key_bytes) =
            crypto::generate_x25519_keypair().map_err(DoubleRatchetError::Crypto)?;

        Ok(KeyPair {
            public_key: PublicKey::from_wire(&public_key_bytes)
                .expect("generate_x25519_keypair always produces a 32-byte key"),
            private_key: Secret::from_slice(&private_key_bytes)
                .expect("x25519 key is always 32 bytes"),
        })
    }

    /// Sign a pre-key with the identity key using XEdDSA.
    /// This allows signing with an X25519 identity key (as OMEMO/Signal requires).
    /// Per libsignal convention, the message signed is the 33-byte key (0x05 prefix + 32 bytes).
    pub fn sign_pre_key(
        identity_private_key: &[u8],
        pre_key_public: &[u8],
    ) -> Result<Vec<u8>, DoubleRatchetError> {
        let prefixed_key = crypto::encode_public_key_with_prefix(pre_key_public);
        crypto::xeddsa_sign(identity_private_key, &prefixed_key).map_err(|e| {
            DoubleRatchetError::InvalidSignature(format!("XEdDSA signing failed: {}", e))
        })
    }

    /// Verify a pre-key signature using XEdDSA.
    /// Verifies that the signed prekey was signed by the holder of the X25519 identity key.
    pub fn verify_pre_key(
        identity_public_key: &[u8],
        pre_key_public: &[u8],
        signature: &[u8],
    ) -> Result<(), DoubleRatchetError> {
        let identity_key_32 = if identity_public_key.len() == 33 && identity_public_key[0] == 0x05 {
            &identity_public_key[1..]
        } else {
            identity_public_key
        };
        let prefixed_spk = crypto::encode_public_key_with_prefix(pre_key_public);
        if crypto::xeddsa_verify(identity_key_32, &prefixed_spk, signature).is_ok() {
            return Ok(());
        }
        let raw_spk = if pre_key_public.len() == 33 && pre_key_public[0] == 0x05 {
            &pre_key_public[1..]
        } else {
            pre_key_public
        };
        crypto::xeddsa_verify(identity_key_32, raw_spk, signature).map_err(|e| {
            DoubleRatchetError::InvalidSignature(format!("XEdDSA verification failed: {}", e))
        })
    }

    /// Perform X3DH key agreement as the initiator with a provided ephemeral key
    #[cfg(test)]
    pub fn key_agreement_initiator_with_ephemeral(
        identity_key_pair: &KeyPair,
        their_identity_key: &[u8],
        their_signed_pre_key: &[u8],
        their_one_time_pre_key: Option<&[u8]>,
        ephemeral_key: &EphemeralPrivateKey,
    ) -> Result<Vec<u8>, DoubleRatchetError> {
        let spk_b = PublicKey::from_wire(their_signed_pre_key).ok_or_else(|| {
            DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                "invalid signed prekey".to_string(),
            ))
        })?;
        let ik_b = PublicKey::from_wire(their_identity_key).ok_or_else(|| {
            DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                "invalid identity key".to_string(),
            ))
        })?;

        // DH1 = DH(IKa, SPKb)
        let dh1 = crypto::x25519_diffie_hellman(&identity_key_pair.private_key, &spk_b)
            .map_err(DoubleRatchetError::Crypto)?;

        // DH2 = DH(EKa, IKb)
        let dh2 = crypto::x25519_diffie_hellman(&ephemeral_key.0, &ik_b)
            .map_err(DoubleRatchetError::Crypto)?;

        // DH3 = DH(EKa, SPKb)
        let dh3 = crypto::x25519_diffie_hellman(&ephemeral_key.0, &spk_b)
            .map_err(DoubleRatchetError::Crypto)?;

        // DH4 = DH(EKa, OPKb) (if OPKb exists)
        let dh4 = if let Some(their_one_time_pre_key) = their_one_time_pre_key {
            let opk_b = PublicKey::from_wire(their_one_time_pre_key).ok_or_else(|| {
                DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                    "invalid one-time prekey".to_string(),
                ))
            })?;
            Some(
                crypto::x25519_diffie_hellman(&ephemeral_key.0, &opk_b)
                    .map_err(DoubleRatchetError::Crypto)?,
            )
        } else {
            None
        };

        // Per Signal spec: IKM = F || DH1 || DH2 || DH3 [|| DH4]
        // where F = 0xFF repeated 32 times
        let mut ikm = vec![0xFFu8; 32];
        ikm.extend_from_slice(&dh1);
        ikm.extend_from_slice(&dh2);
        ikm.extend_from_slice(&dh3);
        if let Some(ref dh4) = dh4 {
            ikm.extend_from_slice(dh4);
        }

        // HKDF(salt=0x00*32, IKM, info="WhisperText", L=32)
        // info="WhisperText" matches libsignal's X3DH key derivation
        let salt = vec![0u8; 32];
        let shared_secret = crypto::hkdf_derive(Salt(&salt), Ikm(&ikm), b"WhisperText", 32)
            .map_err(DoubleRatchetError::Crypto)?;

        Ok(shared_secret)
    }

    /// Perform X3DH key agreement as the recipient
    pub fn key_agreement_recipient(
        identity_key_pair: &KeyPair,
        their_identity_key: &[u8],
        signed_pre_key_pair: &KeyPair,
        one_time_pre_key_pair: Option<&KeyPair>,
        their_ephemeral_key: &[u8],
    ) -> Result<Vec<u8>, DoubleRatchetError> {
        let ik_a = PublicKey::from_wire(their_identity_key).ok_or_else(|| {
            DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                "invalid identity key".to_string(),
            ))
        })?;
        let ek_a = PublicKey::from_wire(their_ephemeral_key).ok_or_else(|| {
            DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                "invalid ephemeral key".to_string(),
            ))
        })?;

        // DH1 = DH(SPKb, IKa)
        let dh1 = crypto::x25519_diffie_hellman(&signed_pre_key_pair.private_key, &ik_a)
            .map_err(DoubleRatchetError::Crypto)?;

        // DH2 = DH(IKb, EKa)
        let dh2 = crypto::x25519_diffie_hellman(&identity_key_pair.private_key, &ek_a)
            .map_err(DoubleRatchetError::Crypto)?;

        // DH3 = DH(SPKb, EKa)
        let dh3 = crypto::x25519_diffie_hellman(&signed_pre_key_pair.private_key, &ek_a)
            .map_err(DoubleRatchetError::Crypto)?;

        // DH4 = DH(OPKb, EKa) (if OPKb exists)
        let dh4 = if let Some(one_time_pre_key_pair) = one_time_pre_key_pair {
            Some(
                crypto::x25519_diffie_hellman(&one_time_pre_key_pair.private_key, &ek_a)
                    .map_err(DoubleRatchetError::Crypto)?,
            )
        } else {
            None
        };

        // Per Signal spec: IKM = F || DH1 || DH2 || DH3 [|| DH4]
        // where F = 0xFF repeated 32 times
        let mut ikm = vec![0xFFu8; 32];
        ikm.extend_from_slice(&dh1);
        ikm.extend_from_slice(&dh2);
        ikm.extend_from_slice(&dh3);
        if let Some(ref dh4) = dh4 {
            ikm.extend_from_slice(dh4);
        }

        // HKDF(salt=0x00*32, IKM, info="WhisperText", L=32)
        // info="WhisperText" matches libsignal's X3DH key derivation
        let salt = vec![0u8; 32];
        let shared_key = crypto::hkdf_derive(Salt(&salt), Ikm(&ikm), b"WhisperText", 32)
            .map_err(DoubleRatchetError::Crypto)?;

        Ok(shared_key)
    }
}

impl DoubleRatchet {
    /// Create a new Double Ratchet session as the initiator with deterministic ephemeral key
    #[cfg(test)]
    pub fn new_session_initiator_with_ephemeral(
        local_identity_key_pair: KeyPair,
        remote_identity_key: Vec<u8>,
        remote_signed_prekey: Vec<u8>,
        remote_one_time_prekey: Option<Vec<u8>>,
        ephemeral_key: EphemeralPrivateKey,
        local_device_id: DeviceId,
        remote_device_id: DeviceId,
        remote_jid: String,
    ) -> Result<RatchetState, DoubleRatchetError> {
        // Perform X3DH key agreement with the provided ephemeral key
        let shared_secret = X3DHProtocol::key_agreement_initiator_with_ephemeral(
            &local_identity_key_pair,
            &remote_identity_key,
            &remote_signed_prekey,
            remote_one_time_prekey.as_deref(),
            &ephemeral_key,
        )?;

        // Create symmetric session state for both initiator and recipient.
        // The initiator branch ignores the last Vec<u8> parameter (establishing_base_key: None),
        // so we pass an empty vec as a harmless placeholder.
        Self::create_symmetric_session_state(
            shared_secret,
            local_identity_key_pair,
            remote_identity_key,
            remote_signed_prekey,
            vec![], // initiator: establishing_base_key is None, this value is unused
            true,   // is_initiator
            local_device_id,
            remote_device_id,
            remote_jid,
        )
    }

    /// Maximum number of message keys to skip (DoS protection)
    const MAX_SKIP: u32 = 1000;

    /// Create a symmetric session state that both initiator and recipient can use
    /// Per Signal spec:
    /// - Initiator: sets remote_ratchet_key = Bob's signed prekey, does a sending DH ratchet step
    /// - Recipient: sets ratchet_key_pair = signed prekey, waits for first message to trigger DH ratchet
    fn create_symmetric_session_state(
        shared_secret: Vec<u8>,
        local_identity_key_pair: KeyPair,
        remote_identity_key: Vec<u8>,
        remote_signed_prekey: Vec<u8>,
        ephemeral_key: Vec<u8>,
        is_initiator: bool,
        local_device_id: DeviceId,
        remote_device_id: DeviceId,
        remote_jid: String,
    ) -> Result<RatchetState, DoubleRatchetError> {
        if is_initiator {
            // Alice (initiator):
            // - root_key = SK (shared secret from X3DH)
            // - remote_ratchet_key = Bob's signed prekey (SPKb)
            // - Perform an initial sending DH ratchet step:
            //   Generate new ratchet keypair, DH(new_ratchet, SPKb) -> KDF_RK -> (new_root, send_chain)
            let ratchet_key_pair = X3DHProtocol::generate_key_pair()?;

            // DH between our new ratchet key and their signed prekey
            let spk_pk = PublicKey::from_wire(&remote_signed_prekey).ok_or_else(|| {
                DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                    "invalid remote signed prekey for ratchet init".to_string(),
                ))
            })?;
            let dh_output = crypto::x25519_diffie_hellman(&ratchet_key_pair.private_key, &spk_pk)
                .map_err(DoubleRatchetError::Crypto)?;

            // KDF_RK(SK, DH) -> (root_key, send_chain_key)
            let kdf_output =
                crypto::hkdf_derive(Salt(&shared_secret), Ikm(&dh_output), b"WhisperRatchet", 64)
                    .map_err(DoubleRatchetError::Crypto)?;
            let root_key =
                RootKey::from_slice(&kdf_output[..32]).expect("KDF output is always 32 bytes");
            let send_chain_key =
                ChainKey::from_slice(&kdf_output[32..64]).expect("KDF output is always 32 bytes");

            let state = RatchetState {
                initialized: true,
                is_initiator: true,
                remote_identity_key,
                local_identity_key_pair,
                root_key,
                send_chain_key,
                receive_chain_key: ChainKey::from_slice(&[0u8; 32]).unwrap(), // Not yet established; set on first DH ratchet from Bob
                ratchet_key_pair,
                remote_ratchet_key: remote_signed_prekey,
                prev_remote_ratchet_key: vec![],
                send_message_number: 0,
                receive_message_number: 0,
                prev_receive_message_number: 0,
                prev_send_message_number: 0,
                skipped_message_keys: std::collections::HashMap::new(),
                local_device_id,
                remote_device_id,
                remote_jid: BareJid::parse(&remote_jid)
                    .map_err(|_| {
                        DoubleRatchetError::InvalidMessageFormat("Invalid remote JID".into())
                    })?
                    .to_string(),
                // Initiator side: we chose the base key, so there is nothing to
                // recognise on the way back in.
                establishing_base_key: None,
            };

            Ok(state)
        } else {
            // Bob (recipient):
            // - root_key = SK (shared secret from X3DH)
            // - ratchet_key_pair = signed prekey (SPKb) — this is what Alice will DH against
            // - Receive chain not yet set; will be established when we receive Alice's first message
            //   which triggers a DH ratchet using her ratchet key

            // Bob's ratchet key IS the signed prekey (Alice will send her ratchet key in first message)
            // We need to find the signed prekey private key - it's passed via ephemeral_key param
            // Actually for recipient, the ratchet key pair is the signed prekey pair
            // The caller (new_session_recipient) passes the signed prekey pair info
            // We'll use a placeholder - the DH ratchet on first received message will replace this
            let state = RatchetState {
                initialized: true,
                is_initiator: false,
                remote_identity_key,
                local_identity_key_pair,
                root_key: RootKey::from_slice(&shared_secret)
                    .expect("X3DH shared secret is 32 bytes"),
                send_chain_key: ChainKey::from_slice(&[0u8; 32]).unwrap(), // Not yet established
                receive_chain_key: ChainKey::from_slice(&[0u8; 32]).unwrap(), // Not yet established
                ratchet_key_pair: KeyPair {
                    public_key: PublicKey::new([0u8; 32]), // placeholder — overwritten by new_session_recipient
                    private_key: Secret::new([0u8; 32]),
                },
                remote_ratchet_key: ephemeral_key.clone(), // Alice's ephemeral key (or her first ratchet key)
                prev_remote_ratchet_key: vec![],
                send_message_number: 0,
                receive_message_number: 0,
                prev_receive_message_number: 0,
                prev_send_message_number: 0,
                skipped_message_keys: std::collections::HashMap::new(),
                local_device_id,
                remote_device_id,
                remote_jid: BareJid::parse(&remote_jid)
                    .map_err(|_| {
                        DoubleRatchetError::InvalidMessageFormat("Invalid remote JID".into())
                    })?
                    .to_string(),
                // Recipient side: `ephemeral_key` here IS the initiator's base
                // key from the PreKeySignalMessage (decrypt.rs passes
                // `prekey_msg.base_key` straight through).  Record it so
                // retransmitted PreKey headers can be matched to this session.
                establishing_base_key: Some(ephemeral_key),
            };

            Ok(state)
        }
    }

    /// Create a new Double Ratchet session as the recipient
    pub fn new_session_recipient(
        local_identity_key_pair: KeyPair,
        remote_identity_key: Vec<u8>,
        local_signed_prekey_pair: KeyPair,
        local_one_time_prekey_pair: Option<KeyPair>,
        remote_ephemeral_key: Vec<u8>,
        local_device_id: DeviceId,
        remote_device_id: DeviceId,
        remote_jid: String,
    ) -> Result<RatchetState, DoubleRatchetError> {
        // Perform X3DH key agreement
        let shared_secret = X3DHProtocol::key_agreement_recipient(
            &local_identity_key_pair,
            &remote_identity_key,
            &local_signed_prekey_pair,
            local_one_time_prekey_pair.as_ref(),
            &remote_ephemeral_key,
        )?;

        // For recipient: ratchet_key_pair = signed prekey (Bob's SPK)
        // Alice's ephemeral key is the remote_ratchet_key (she DH'd against our SPK)
        let spk_pair = local_signed_prekey_pair.clone();

        let mut state = Self::create_symmetric_session_state(
            shared_secret,
            local_identity_key_pair,
            remote_identity_key,
            local_signed_prekey_pair.public_key.to_vec(),
            remote_ephemeral_key,
            false, // is_initiator
            local_device_id,
            remote_device_id,
            remote_jid,
        )?;

        // Set ratchet key pair to signed prekey pair (Bob uses SPK as initial ratchet key)
        state.ratchet_key_pair = spk_pair;

        Ok(state)
    }

    /// Skip message keys up to a specific counter
    fn skip_message_keys(state: &mut RatchetState, target: u32) -> Result<(), DoubleRatchetError> {
        if target > state.receive_message_number.saturating_add(Self::MAX_SKIP) {
            return Err(DoubleRatchetError::InvalidMessageFormat(format!(
                "Too many skipped messages: {} (max {})",
                target - state.receive_message_number,
                Self::MAX_SKIP
            )));
        }

        while state.receive_message_number < target {
            let current_counter = state.receive_message_number;
            // derive_next_receiving_key increments receive_message_number
            let message_key = Self::derive_next_receiving_key(state);

            // Store with the counter value this key corresponds to
            let key = (state.remote_ratchet_key.clone(), current_counter);
            state
                .skipped_message_keys
                .insert(key, MessageKey(message_key.0));
        }

        // Prune oldest skipped keys if we exceed the storage cap
        const MAX_STORED_SKIPPED_KEYS: usize = 3000;
        while state.skipped_message_keys.len() > MAX_STORED_SKIPPED_KEYS {
            // Remove the entry with the lowest counter (oldest skipped key)
            if let Some(oldest_key) = state
                .skipped_message_keys
                .keys()
                .min_by_key(|(_, counter)| *counter)
                .cloned()
            {
                state.skipped_message_keys.remove(&oldest_key);
            } else {
                break;
            }
        }

        Ok(())
    }

    /// Derive the next sending key using HMAC-based chain ratchet (Signal spec)
    /// message_key = HMAC-SHA256(chain_key, 0x01)
    /// next_chain_key = HMAC-SHA256(chain_key, 0x02)
    fn derive_next_sending_key(state: &mut RatchetState) -> MessageKey {
        let message_key = MessageKey::from_slice(
            &crypto::hmac_sha256(state.send_chain_key.expose_secret(), &[0x01])
                .expect("HMAC-SHA256 cannot fail"),
        )
        .expect("HMAC-SHA256 output is always 32 bytes");
        state.send_chain_key = ChainKey::from_slice(
            &crypto::hmac_sha256(state.send_chain_key.expose_secret(), &[0x02])
                .expect("HMAC-SHA256 cannot fail"),
        )
        .expect("HMAC-SHA256 output is always 32 bytes");
        message_key
    }

    /// Derive the next receiving key using HMAC-based chain ratchet (Signal spec)
    fn derive_next_receiving_key(state: &mut RatchetState) -> MessageKey {
        let message_key = MessageKey::from_slice(
            &crypto::hmac_sha256(state.receive_chain_key.expose_secret(), &[0x01])
                .expect("HMAC-SHA256 cannot fail"),
        )
        .expect("HMAC-SHA256 output is always 32 bytes");
        state.receive_chain_key = ChainKey::from_slice(
            &crypto::hmac_sha256(state.receive_chain_key.expose_secret(), &[0x02])
                .expect("HMAC-SHA256 cannot fail"),
        )
        .expect("HMAC-SHA256 output is always 32 bytes");
        state.receive_message_number += 1;
        message_key
    }

    /// Perform a DH ratchet step
    fn dh_ratchet(
        state: &mut RatchetState,
        their_ratchet_key: &[u8],
        previous_counter: u32,
    ) -> Result<(), DoubleRatchetError> {
        // Drain the remaining previous receiving chain before ratcheting.
        // remote_ratchet_key is still the OLD key here, so skip keys are stored
        // under (old_key, counter) and remain reachable for delayed messages.
        Self::skip_message_keys(state, previous_counter)?;

        // Save previous state
        state.prev_remote_ratchet_key = state.remote_ratchet_key.clone();
        state.remote_ratchet_key = their_ratchet_key.to_vec();
        state.prev_receive_message_number = state.receive_message_number;
        state.receive_message_number = 0;

        // DH for receiving chain: DH(our_ratchet_private, their_new_ratchet_public)
        let rk_pk = PublicKey::from_wire(their_ratchet_key).ok_or_else(|| {
            DoubleRatchetError::Crypto(crypto::CryptoError::InvalidInput(
                "invalid ratchet key in DH ratchet".to_string(),
            ))
        })?;
        let dh_recv = crypto::x25519_diffie_hellman(&state.ratchet_key_pair.private_key, &rk_pk)
            .map_err(DoubleRatchetError::Crypto)?;

        // KDF_RK(root_key, dh_recv) -> (new_root_key, receive_chain_key)
        let kdf_recv = crypto::hkdf_derive(
            Salt::from(&state.root_key),
            Ikm(&dh_recv),
            b"WhisperRatchet",
            64,
        )
        .map_err(DoubleRatchetError::Crypto)?;
        state.root_key = RootKey::from_slice(&kdf_recv[..32]).expect("KDF is always 64 bytes");
        state.receive_chain_key =
            ChainKey::from_slice(&kdf_recv[32..64]).expect("KDF is always 64 bytes");

        // Generate a new ratchet key pair for sending
        state.ratchet_key_pair = X3DHProtocol::generate_key_pair()?;
        state.prev_send_message_number = state.send_message_number;
        state.send_message_number = 0;

        // DH for sending chain: DH(new_ratchet_private, their_ratchet_public) — reuse rk_pk
        let dh_send = crypto::x25519_diffie_hellman(&state.ratchet_key_pair.private_key, &rk_pk)
            .map_err(DoubleRatchetError::Crypto)?;

        // KDF_RK(root_key, dh_send) -> (new_root_key, send_chain_key)
        let kdf_send = crypto::hkdf_derive(
            Salt::from(&state.root_key),
            Ikm(&dh_send),
            b"WhisperRatchet",
            64,
        )
        .map_err(DoubleRatchetError::Crypto)?;
        state.root_key = RootKey::from_slice(&kdf_send[..32]).expect("KDF is always 64 bytes");
        state.send_chain_key =
            ChainKey::from_slice(&kdf_send[32..64]).expect("KDF is always 64 bytes");

        Ok(())
    }

    /// Encrypt a message key for transport using Signal wire format.
    /// Produces a serialized SignalMessage (version || protobuf || mac).
    pub fn encrypt_key(
        state: &mut RatchetState,
        key: &[u8],
    ) -> Result<Vec<u8>, DoubleRatchetError> {
        if state.send_message_number == u32::MAX {
            return Err(DoubleRatchetError::InvalidMessageFormat(
                "Sending counter exhausted".into(),
            ));
        }

        // Derive a message key from the sending chain
        let message_key = Self::derive_next_sending_key(state);

        // Expand message_key via HKDF to get (cipher_key, mac_key, iv)
        // Per Signal spec: HKDF(message_key, salt="", info="WhisperMessageKeys", L=80)
        let expanded = crypto::hkdf_derive(
            Salt(&[]),
            Ikm(message_key.expose_secret()),
            b"WhisperMessageKeys",
            80,
        )
        .map_err(DoubleRatchetError::Crypto)?;
        let cipher_key = AesCbcKey::from_slice(&expanded[..32]).expect("HKDF output is 80 bytes");
        let mac_key = &expanded[32..64]; // HMAC-SHA256 key
        let iv = CbcIv::from_slice(&expanded[64..80]).expect("HKDF output is 80 bytes");

        // Encrypt the OMEMO message key using AES-256-CBC with PKCS7 padding
        let ciphertext = crypto::aes_256_cbc_encrypt(&cipher_key, &iv, key)
            .map_err(DoubleRatchetError::Crypto)?;

        // Build a SignalMessage in wire format
        let signal_msg = crate::xmpp_omemo::omemo::wire::SignalMessage {
            ratchet_key: state.ratchet_key_pair.public_key.to_vec(),
            counter: state.send_message_number,
            previous_counter: state.prev_send_message_number,
            ciphertext,
            mac: Vec::new(), // computed during serialization
        };

        // Increment the message number after using it
        state.send_message_number += 1;

        // Serialize with MAC including identity keys
        let result = signal_msg.serialize_with_identity(
            mac_key,
            state.local_identity_key_pair.public_key.as_ref(),
            &state.remote_identity_key,
        );

        Ok(result)
    }

    /// Decrypt a message key from Signal wire format (SignalMessage or PreKeySignalMessage).
    pub fn decrypt_key(
        state: &mut RatchetState,
        encrypted_key: &[u8],
    ) -> Result<Vec<u8>, DoubleRatchetError> {
        // Try parsing as PreKeySignalMessage first, then SignalMessage
        let (signal_msg, raw_msg_bytes) = if let Some(prekey_msg) =
            crate::xmpp_omemo::omemo::wire::PreKeySignalMessage::deserialize(encrypted_key)
        {
            // For PreKey messages, extract the inner SignalMessage raw bytes for MAC verification
            let inner_bytes = prekey_msg.raw_message_bytes.clone();
            (prekey_msg.message, inner_bytes)
        } else if let Some(msg) =
            crate::xmpp_omemo::omemo::wire::SignalMessage::deserialize(encrypted_key)
        {
            // The raw bytes for MAC verification are the full encrypted_key
            (msg, encrypted_key.to_vec())
        } else {
            return Err(DoubleRatchetError::InvalidMessageFormat(
                "Failed to parse encrypted key as SignalMessage or PreKeySignalMessage".to_string(),
            ));
        };

        if signal_msg.counter == u32::MAX {
            return Err(DoubleRatchetError::InvalidMessageFormat(
                "Receiving counter exhausted".into(),
            ));
        }

        // Work on a candidate clone; commit to state only on full success so
        // no mutation is visible to the caller on any error path.
        let mut candidate = state.clone();

        let skip_index = (signal_msg.ratchet_key.clone(), signal_msg.counter);
        let message_key =
            if let Some(stored_key) = candidate.skipped_message_keys.remove(&skip_index) {
                stored_key
            } else {
                if !signal_msg.ratchet_key.is_empty()
                    && signal_msg.ratchet_key != candidate.remote_ratchet_key
                {
                    Self::dh_ratchet(
                        &mut candidate,
                        &signal_msg.ratchet_key,
                        signal_msg.previous_counter,
                    )?
                }

                if signal_msg.counter < candidate.receive_message_number {
                    return Err(DoubleRatchetError::InvalidMessageFormat(format!(
                        "Message counter {} is too old (already processed, no stored key)",
                        signal_msg.counter
                    )));
                }

                if signal_msg.counter > candidate.receive_message_number {
                    if signal_msg.counter - candidate.receive_message_number > Self::MAX_SKIP {
                        return Err(DoubleRatchetError::InvalidMessageFormat(
                            "Too many skipped messages in decrypt_key".to_string(),
                        ));
                    }
                    while candidate.receive_message_number < signal_msg.counter {
                        let current_counter = candidate.receive_message_number;
                        let skipped_key = Self::derive_next_receiving_key(&mut candidate);
                        let skip_index = (signal_msg.ratchet_key.clone(), current_counter);
                        candidate
                            .skipped_message_keys
                            .insert(skip_index, skipped_key);
                    }
                }

                Self::derive_next_receiving_key(&mut candidate)
            };

        // Expand message_key via HKDF to get (cipher_key, mac_key, iv)
        let expanded = crypto::hkdf_derive(
            Salt(&[]),
            Ikm(message_key.expose_secret()),
            b"WhisperMessageKeys",
            80,
        )
        .map_err(DoubleRatchetError::Crypto)?;
        let cipher_key = AesCbcKey::from_slice(&expanded[..32]).expect("HKDF output is 80 bytes");
        let mac_key = &expanded[32..64]; // HMAC-SHA256 key
        let iv = CbcIv::from_slice(&expanded[64..80]).expect("HKDF output is 80 bytes");

        // Verify MAC before decryption.
        // MAC covers: sender_identity(33) || receiver_identity(33) || version || protobuf
        // The raw_msg_bytes contain version || proto || mac[8]
        // Explicit guard so that MAC verification is never skipped (C2 fix).
        if raw_msg_bytes.len() < 1 + crate::xmpp_omemo::omemo::wire::MAC_LENGTH {
            return Err(DoubleRatchetError::InvalidMessageFormat(format!(
                "Message too short for MAC: {} bytes (need >= {})",
                raw_msg_bytes.len(),
                1 + crate::xmpp_omemo::omemo::wire::MAC_LENGTH
            )));
        }
        let msg_without_mac =
            &raw_msg_bytes[..raw_msg_bytes.len() - crate::xmpp_omemo::omemo::wire::MAC_LENGTH];

        let sender_prefixed = crypto::encode_public_key_with_prefix(&candidate.remote_identity_key);
        let receiver_prefixed = crypto::encode_public_key_with_prefix(
            candidate.local_identity_key_pair.public_key.as_ref(),
        );
        let mut mac_input = Vec::with_capacity(
            sender_prefixed.len() + receiver_prefixed.len() + msg_without_mac.len(),
        );
        mac_input.extend_from_slice(&sender_prefixed);
        mac_input.extend_from_slice(&receiver_prefixed);
        mac_input.extend_from_slice(msg_without_mac);

        // verify_and_authenticate returns AuthenticatedSignalMessage, proving
        // MAC was checked before we access signal_msg.ciphertext for decryption.
        let authenticated = crate::xmpp_omemo::omemo::wire::verify_and_authenticate(
            mac_key, &mac_input, signal_msg,
        )
        .map_err(|_| {
            DoubleRatchetError::Crypto(crypto::CryptoError::AesGcm(
                "MAC verification failed".to_string(),
            ))
        })?;

        // Decrypt using AES-256-CBC with PKCS7 padding.
        // `authenticated.0.ciphertext` can only be reached through the MAC-verified wrapper.
        let key = crypto::aes_256_cbc_decrypt(&cipher_key, &iv, &authenticated.0.ciphertext)
            .map_err(DoubleRatchetError::Crypto)?;

        // All checks passed — commit candidate to state.
        *state = candidate;
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    fn state_snapshot(state: &super::RatchetState) -> serde_json::Value {
        let mut snapshot = state.clone();
        let mut skipped: Vec<_> = std::mem::take(&mut snapshot.skipped_message_keys)
            .into_iter()
            .collect();
        skipped.sort_by(|a, b| a.0.cmp(&b.0));
        serde_json::json!({"state":snapshot,"skipped":skipped})
    }
    use super::*;

    fn paired_sessions() -> (RatchetState, RatchetState) {
        let alice_identity = X3DHProtocol::generate_key_pair().unwrap();
        let bob_identity = X3DHProtocol::generate_key_pair().unwrap();
        let bob_spk = X3DHProtocol::generate_key_pair().unwrap();
        let bob_opk = X3DHProtocol::generate_key_pair().unwrap();
        let ephemeral = X3DHProtocol::generate_key_pair().unwrap();

        let alice = DoubleRatchet::new_session_initiator_with_ephemeral(
            alice_identity.clone(),
            bob_identity.public_key.to_vec(),
            bob_spk.public_key.to_vec(),
            Some(bob_opk.public_key.to_vec()),
            EphemeralPrivateKey(ephemeral.private_key.clone()),
            DeviceId::from(1), // local device id
            DeviceId::from(2), // remote device id
            "bob@example.com".to_string(),
        )
        .unwrap();

        let bob = DoubleRatchet::new_session_recipient(
            bob_identity,
            alice_identity.public_key.to_vec(),
            bob_spk,
            Some(bob_opk),
            ephemeral.public_key.to_vec(),
            DeviceId::from(2),
            DeviceId::from(1),
            "alice@example.com".to_string(),
        )
        .unwrap();

        (alice, bob)
    }

    #[test]
    fn test_decrypt_key_out_of_order_delivery() {
        // Regression test: messages that arrive out of order must still decrypt
        // using the stored skipped message keys. Previously decrypt_key stored
        // skipped keys but never consulted them, so delayed messages were lost.
        let (mut alice, mut bob) = paired_sessions();

        let k0 = vec![0x10u8; 32];
        let k1 = vec![0x11u8; 32];
        let k2 = vec![0x12u8; 32];

        let m0 = DoubleRatchet::encrypt_key(&mut alice, &k0).unwrap();
        let m1 = DoubleRatchet::encrypt_key(&mut alice, &k1).unwrap();
        let m2 = DoubleRatchet::encrypt_key(&mut alice, &k2).unwrap();

        // Deliver out of order: m2 first, then m0, then m1.
        let d2 = DoubleRatchet::decrypt_key(&mut bob, &m2).unwrap();
        assert_eq!(d2, k2, "newest message should decrypt");

        let d0 = DoubleRatchet::decrypt_key(&mut bob, &m0).unwrap();
        assert_eq!(d0, k0, "delayed message 0 should decrypt via skipped key");

        let d1 = DoubleRatchet::decrypt_key(&mut bob, &m1).unwrap();
        assert_eq!(d1, k1, "delayed message 1 should decrypt via skipped key");
    }

    #[test]
    fn test_decrypt_key_replay_rejected() {
        // A replayed message (already consumed, no stored key) must be rejected
        // rather than silently advancing/desyncing the receiving chain.
        let (mut alice, mut bob) = paired_sessions();

        let k0 = vec![0x20u8; 32];
        let m0 = DoubleRatchet::encrypt_key(&mut alice, &k0).unwrap();

        assert_eq!(DoubleRatchet::decrypt_key(&mut bob, &m0).unwrap(), k0);
        // Replaying the same message must error (key already consumed).
        assert!(DoubleRatchet::decrypt_key(&mut bob, &m0).is_err());

        // The chain must still be usable for the next legitimate message.
        let k1 = vec![0x21u8; 32];
        let m1 = DoubleRatchet::encrypt_key(&mut alice, &k1).unwrap();
        assert_eq!(DoubleRatchet::decrypt_key(&mut bob, &m1).unwrap(), k1);
    }

    #[test]
    fn test_dh_ratchet_drains_previous_chain() {
        // Scenario: Alice sends m0 (received by Bob, establishes chains), then m1 and m2
        // (delayed).  Bob replies r0 forcing Alice to ratchet.  Alice then sends m3 on
        // the new chain (previous_counter=3).  When Bob receives m3, the DH ratchet
        // must pre-derive and store keys for (old_ratchet_key, 1) and (old_ratchet_key, 2)
        // so the delayed m1 and m2 can still be decrypted afterwards.
        let (mut alice, mut bob) = paired_sessions();

        let k0 = vec![0x10u8; 32];
        let k1 = vec![0x11u8; 32];
        let k2 = vec![0x12u8; 32];
        let k3 = vec![0x13u8; 32];

        // Step 1: Alice sends m0; Bob receives it immediately (seeds Bob's chains).
        let m0 = DoubleRatchet::encrypt_key(&mut alice, &k0).unwrap();
        let d0 = DoubleRatchet::decrypt_key(&mut bob, &m0).unwrap();
        assert_eq!(d0, k0);

        // Step 2: Alice sends m1, m2 — these are delayed (Bob hasn't received them).
        let m1 = DoubleRatchet::encrypt_key(&mut alice, &k1).unwrap();
        let m2 = DoubleRatchet::encrypt_key(&mut alice, &k2).unwrap();

        // Step 3: Bob sends r0; Alice receives it, triggering Alice's DH ratchet.
        //         Alice's prev_send_message_number becomes 3 (sent m0, m1, m2).
        let kr = vec![0xBBu8; 32];
        let r0 = DoubleRatchet::encrypt_key(&mut bob, &kr).unwrap();
        let dr = DoubleRatchet::decrypt_key(&mut alice, &r0).unwrap();
        assert_eq!(dr, kr);

        // Step 4: Alice sends m3 on her NEW chain (previous_counter = 3).
        let m3 = DoubleRatchet::encrypt_key(&mut alice, &k3).unwrap();

        // Step 5: Bob receives m3.  Bob's receive_message_number == 1 (received m0 only).
        //         m3 carries a new ratchet key → DH ratchet fires.
        //         With the fix, the ratchet pre-stores keys for (R_A, 1) and (R_A, 2)
        //         before discarding the old receiving chain.
        let d3 = DoubleRatchet::decrypt_key(&mut bob, &m3).unwrap();
        assert_eq!(d3, k3, "m3 on new chain must decrypt");

        // Step 6: The delayed m1 and m2 now arrive.
        let d1 = DoubleRatchet::decrypt_key(&mut bob, &m1).unwrap();
        assert_eq!(
            d1, k1,
            "delayed m1 must decrypt via pre-stored key from old chain"
        );

        let d2 = DoubleRatchet::decrypt_key(&mut bob, &m2).unwrap();
        assert_eq!(
            d2, k2,
            "delayed m2 must decrypt via pre-stored key from old chain"
        );
    }

    #[test]
    fn test_decrypt_key_mac_check_is_unconditional() {
        // Structural guard: verify that MAC failure always produces
        // InvalidMessageFormatError / CryptoError, never falls through to
        // decryption.  This is a regression guard for the fail-open `if len > 8`
        // that previously had no else-branch (C2).
        //
        // Note: SignalMessage::deserialize already enforces len >= 1 + MAC_LENGTH,
        // so the fail-open path is unreachable through normal deserialization.
        // This test confirms the MAC is checked for every valid-length message.
        let (mut alice, mut bob) = paired_sessions();

        let k0 = vec![0x55u8; 32];
        let mut msg = DoubleRatchet::encrypt_key(&mut alice, &k0).unwrap();

        // Flip a bit in the MAC region — decrypt must fail with a crypto error,
        // not succeed (which would mean MAC was bypassed).
        let last = msg.len() - 1;
        msg[last] ^= 0xFF;

        let result = DoubleRatchet::decrypt_key(&mut bob, &msg);
        assert!(result.is_err(), "corrupted MAC must always be rejected");
        // Verify it's a MAC error (CryptoError), not a downstream decrypt error.
        assert!(
            matches!(result.unwrap_err(), DoubleRatchetError::Crypto(_)),
            "corrupted MAC must be rejected at the MAC layer, not by AES-CBC"
        );
    }

    #[test]
    fn test_decrypt_key_corrupted_mac_leaves_state_unchanged() {
        let (mut alice, mut bob) = paired_sessions();

        let k0 = vec![0xABu8; 32];
        let mut msg = DoubleRatchet::encrypt_key(&mut alice, &k0).unwrap();

        // Corrupt the MAC (last 8 bytes) by flipping the final byte.
        let last = msg.len() - 1;
        msg[last] ^= 0xFF;

        let bob_before = bob.clone();

        assert!(
            DoubleRatchet::decrypt_key(&mut bob, &msg).is_err(),
            "corrupted MAC must be rejected"
        );

        assert_eq!(
            bob.receive_message_number,
            bob_before.receive_message_number
        );
        assert_eq!(bob.remote_ratchet_key, bob_before.remote_ratchet_key);
        assert_eq!(bob.receive_chain_key, bob_before.receive_chain_key);
        assert_eq!(bob.root_key, bob_before.root_key);
        assert_eq!(bob.skipped_message_keys, bob_before.skipped_message_keys);

        // Session must still be usable after the rejected message.
        let k1 = vec![0xCDu8; 32];
        let msg1 = DoubleRatchet::encrypt_key(&mut alice, &k1).unwrap();
        assert_eq!(DoubleRatchet::decrypt_key(&mut bob, &msg1).unwrap(), k1);
    }

    #[test]
    fn test_decrypt_key_bogus_ratchet_key_leaves_state_unchanged() {
        // A message whose ratchet_key triggers the DH-ratchet branch must not
        // commit that ratchet step when the MAC then fails.
        let (mut alice, mut bob) = paired_sessions();

        // Consume one message so alice's state is non-trivial.
        let _m0 = DoubleRatchet::encrypt_key(&mut alice, &[0x11u8; 32]).unwrap();

        let bogus_ratchet = X3DHProtocol::generate_key_pair().unwrap();
        let forged = crate::xmpp_omemo::omemo::wire::SignalMessage {
            ratchet_key: bogus_ratchet.public_key.to_vec(),
            counter: 0,
            previous_counter: 0,
            ciphertext: vec![0u8; 32],
            mac: Vec::new(),
        }
        .serialize_with_identity(
            &[0u8; 32], // wrong mac_key → MAC will not verify
            bogus_ratchet.public_key.as_ref(),
            bogus_ratchet.public_key.as_ref(),
        );

        let bob_before = bob.clone();

        assert!(
            DoubleRatchet::decrypt_key(&mut bob, &forged).is_err(),
            "forged ratchet key with bad MAC must be rejected"
        );

        assert_eq!(bob.remote_ratchet_key, bob_before.remote_ratchet_key);
        assert_eq!(bob.root_key, bob_before.root_key);
        assert_eq!(bob.receive_chain_key, bob_before.receive_chain_key);
        assert_eq!(bob.send_chain_key, bob_before.send_chain_key);
        assert_eq!(
            bob.receive_message_number,
            bob_before.receive_message_number
        );
    }

    #[test]
    fn test_sign_verify_pre_key_roundtrip() {
        // This test exercises the EXACT same path as production:
        // 1. Generate identity key pair (raw 32-byte keys)
        // 2. Generate SPK pair (raw 32-byte keys)
        // 3. Sign SPK public key with identity private key
        // 4. Encode keys with 0x05 prefix (as stored in bundle XML)
        // 5. Verify using the 33-byte prefixed keys (as parsed from bundle)
        //
        // This catches any format mismatch between sign and verify paths.
        for i in 0..20 {
            let identity_kp = X3DHProtocol::generate_key_pair().unwrap();
            let spk_kp = X3DHProtocol::generate_key_pair().unwrap();

            // Sign: uses raw 32-byte private key and raw 32-byte SPK public
            let signature = X3DHProtocol::sign_pre_key(
                identity_kp.private_key.expose_secret(),
                spk_kp.public_key.as_ref(),
            )
            .unwrap();
            assert_eq!(signature.len(), 64, "Signature should be 64 bytes");

            // Simulate bundle encoding: identity public and SPK public get 0x05 prefix
            let identity_public_33 =
                crypto::encode_public_key_with_prefix(identity_kp.public_key.as_ref());
            let spk_public_33 = crypto::encode_public_key_with_prefix(spk_kp.public_key.as_ref());
            assert_eq!(identity_public_33.len(), 33);
            assert_eq!(spk_public_33.len(), 33);

            // Verify: uses 33-byte prefixed keys (as parsed from bundle XML)
            let valid =
                X3DHProtocol::verify_pre_key(&identity_public_33, &spk_public_33, &signature)
                    .is_ok();
            assert!(
                valid,
                "SPK signature verification failed on iteration {} (key format mismatch between sign and verify)",
                i
            );
        }
    }
    #[test]
    fn exhausted_counters_fail_without_mutation() {
        let (mut sender, mut receiver) = paired_sessions();
        sender.send_message_number = u32::MAX;
        let before = state_snapshot(&sender);
        assert!(DoubleRatchet::encrypt_key(&mut sender, b"fixture").is_err());
        assert_eq!(state_snapshot(&sender), before);
        receiver.receive_message_number = u32::MAX - 2;
        let before = state_snapshot(&receiver);
        for counter in [u32::MAX - 1, u32::MAX] {
            let wire = crate::xmpp_omemo::omemo::wire::SignalMessage {
                ratchet_key: receiver.remote_ratchet_key.clone(),
                counter,
                previous_counter: 0,
                ciphertext: vec![0; 16],
                mac: vec![],
            }
            .serialize(&[0; 32]);
            assert!(DoubleRatchet::decrypt_key(&mut receiver, &wire).is_err());
            assert_eq!(state_snapshot(&receiver), before);
        }
    }
}
