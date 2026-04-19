//! Noise XX handshake and transport session management for BitChat mesh.
//!
//! Implements `Noise_XX_25519_ChaChaPoly_BLAKE2s` — the same pattern used by
//! the BitChat Swift app (`NoiseEncryptionService.swift`). After a completed
//! handshake both sides derive symmetric transport ciphers for
//! encrypt-then-relay message flow.
//!
//! ## State machine
//!
//! ```text
//! Idle ──initiator──► HandshakeSent ──resp──► Established
//!      ◄─responder─── HandshakeRecv ──init──► Established
//! ```
//!
//! Sessions are keyed by the remote peer's 8-byte [`PeerId`].

use anyhow::{Context, Result, bail};
use snow::{Builder, TransportState, params::NoiseParams};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::packet::PEER_ID_LEN;

/// Noise protocol parameter string matching BitChat's Swift implementation.
const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// A peer's 8-byte identifier derived from their Noise static public key.
pub type PeerId = [u8; PEER_ID_LEN];

/// Per-peer session state.
enum SessionState {
    /// Handshake in progress (initiator or responder side).
    Handshake(Box<snow::HandshakeState>),
    /// Handshake complete; ready to encrypt/decrypt.
    Established(Box<TransportState>),
}

/// Manages Noise XX sessions with multiple BLE peers.
///
/// All methods are async to allow use from Tokio tasks without blocking.
/// The inner state is guarded by a `Mutex` for concurrent access from
/// the BLE receive loop and the outgoing send path.
pub struct NoiseSessionManager {
    /// Our long-term Noise static keypair (Curve25519).
    local_static_key: Vec<u8>,
    /// Sessions indexed by remote peer ID.
    sessions: Arc<Mutex<HashMap<PeerId, SessionState>>>,
}

impl NoiseSessionManager {
    /// Create a manager with a freshly generated Curve25519 static keypair.
    pub fn generate() -> Result<(Self, Vec<u8>)> {
        let params: NoiseParams = NOISE_PARAMS.parse().context("Invalid Noise params")?;
        let builder = Builder::new(params);
        let keypair = builder.generate_keypair().context("Noise keypair generation")?;
        let pubkey = keypair.public.clone();
        let mgr = Self {
            local_static_key: keypair.private.clone(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        };
        Ok((mgr, pubkey))
    }

    /// Create a manager from an existing raw Curve25519 private key (32 bytes).
    pub fn from_private_key(private_key: Vec<u8>) -> Result<Self> {
        if private_key.len() != 32 {
            bail!("Noise private key must be 32 bytes, got {}", private_key.len());
        }
        Ok(Self {
            local_static_key: private_key,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    // ─── Initiator side ─────────────────────────────────────────────────────

    /// Start a Noise XX handshake as the initiator.
    ///
    /// Returns the first handshake message (`e →`) to send to the peer.
    pub async fn initiate_handshake(&self, peer_id: PeerId) -> Result<Vec<u8>> {
        let params: NoiseParams = NOISE_PARAMS.parse()?;
        let mut hs = Builder::new(params)
            .local_private_key(&self.local_static_key)
            .build_initiator()
            .context("Noise initiator build")?;

        let mut msg = vec![0u8; 65536];
        let len = hs.write_message(&[], &mut msg).context("Noise handshake write (init)")?;
        let msg = msg[..len].to_vec();

        let mut sessions = self.sessions.lock().await;
        sessions.insert(peer_id, SessionState::Handshake(Box::new(hs)));

        tracing::debug!("Noise: initiated handshake with peer {:02x?}", &peer_id);
        Ok(msg)
    }

    /// Process the responder's reply and produce our final handshake message.
    ///
    /// After this the session is established on the initiator side.
    pub async fn receive_handshake_response(
        &self,
        peer_id: PeerId,
        resp_msg: &[u8],
    ) -> Result<Vec<u8>> {
        let mut sessions = self.sessions.lock().await;
        let state = sessions
            .get_mut(&peer_id)
            .with_context(|| format!("No handshake in progress for peer {:02x?}", &peer_id))?;

        let hs = match state {
            SessionState::Handshake(hs) => hs,
            SessionState::Established(_) => bail!("Session already established"),
        };

        let mut _payload = vec![0u8; 65536];
        hs.read_message(resp_msg, &mut _payload)
            .context("Noise: failed to read responder message")?;

        let mut final_msg = vec![0u8; 65536];
        let len = hs
            .write_message(&[], &mut final_msg)
            .context("Noise: failed to write final initiator message")?;
        let final_msg = final_msg[..len].to_vec();

        // Transition to transport — take ownership
        let hs_owned = match sessions.remove(&peer_id) {
            Some(SessionState::Handshake(hs)) => hs,
            _ => bail!("Unexpected session state"),
        };
        let transport = hs_owned.into_transport_mode().context("Noise: into transport")?;
        sessions.insert(peer_id, SessionState::Established(Box::new(transport)));

        tracing::info!(
            "Noise: session established with peer {:02x?} (initiator)",
            &peer_id
        );
        Ok(final_msg)
    }

    // ─── Responder side ──────────────────────────────────────────────────────

    /// Process the initiator's first message and return our reply.
    pub async fn receive_handshake_init(
        &self,
        peer_id: PeerId,
        init_msg: &[u8],
    ) -> Result<Vec<u8>> {
        let params: NoiseParams = NOISE_PARAMS.parse()?;
        let mut hs = Builder::new(params)
            .local_private_key(&self.local_static_key)
            .build_responder()
            .context("Noise responder build")?;

        let mut _payload = vec![0u8; 65536];
        hs.read_message(init_msg, &mut _payload)
            .context("Noise: failed to read initiator message")?;

        let mut resp_msg = vec![0u8; 65536];
        let len = hs
            .write_message(&[], &mut resp_msg)
            .context("Noise: failed to write responder message")?;
        let resp_msg = resp_msg[..len].to_vec();

        let mut sessions = self.sessions.lock().await;
        sessions.insert(peer_id, SessionState::Handshake(Box::new(hs)));

        tracing::debug!("Noise: sent handshake response to peer {:02x?}", &peer_id);
        Ok(resp_msg)
    }

    /// Finalise the responder handshake by reading the initiator's final message.
    pub async fn finalize_responder_handshake(
        &self,
        peer_id: PeerId,
        final_msg: &[u8],
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let hs_owned = match sessions.remove(&peer_id) {
            Some(SessionState::Handshake(hs)) => hs,
            _ => bail!("No handshake in progress for peer {:02x?}", &peer_id),
        };

        let mut _payload = vec![0u8; 65536];
        let mut hs = hs_owned;
        hs.read_message(final_msg, &mut _payload)
            .context("Noise: failed to read final initiator message")?;

        let transport = hs.into_transport_mode().context("Noise: into transport")?;
        sessions.insert(peer_id, SessionState::Established(Box::new(transport)));

        tracing::info!(
            "Noise: session established with peer {:02x?} (responder)",
            &peer_id
        );
        Ok(())
    }

    // ─── Encrypt / Decrypt ──────────────────────────────────────────────────

    /// Encrypt `plaintext` for the given peer. Session must be established.
    pub async fn encrypt(&self, peer_id: PeerId, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut sessions = self.sessions.lock().await;
        let transport = match sessions.get_mut(&peer_id) {
            Some(SessionState::Established(t)) => t,
            Some(SessionState::Handshake(_)) => bail!("Handshake not yet complete"),
            None => bail!("No session with peer {:02x?}", &peer_id),
        };

        let mut ciphertext = vec![0u8; plaintext.len() + 16];
        let len = transport
            .write_message(plaintext, &mut ciphertext)
            .context("Noise: encrypt failed")?;
        ciphertext.truncate(len);
        Ok(ciphertext)
    }

    /// Decrypt `ciphertext` from the given peer. Session must be established.
    pub async fn decrypt(&self, peer_id: PeerId, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let mut sessions = self.sessions.lock().await;
        let transport = match sessions.get_mut(&peer_id) {
            Some(SessionState::Established(t)) => t,
            Some(SessionState::Handshake(_)) => bail!("Handshake not yet complete"),
            None => bail!("No session with peer {:02x?}", &peer_id),
        };

        let mut plaintext = vec![0u8; ciphertext.len()];
        let len = transport
            .read_message(ciphertext, &mut plaintext)
            .context("Noise: decrypt failed")?;
        plaintext.truncate(len);
        Ok(plaintext)
    }

    // ─── Session state queries ───────────────────────────────────────────────

    /// Returns `true` if an established (post-handshake) session exists.
    pub async fn is_established(&self, peer_id: PeerId) -> bool {
        let sessions = self.sessions.lock().await;
        matches!(sessions.get(&peer_id), Some(SessionState::Established(_)))
    }

    /// Returns `true` if any session (handshake or established) exists.
    pub async fn has_session(&self, peer_id: PeerId) -> bool {
        self.sessions.lock().await.contains_key(&peer_id)
    }

    /// Remove a session (e.g. after peer disconnects).
    pub async fn remove_session(&self, peer_id: PeerId) {
        self.sessions.lock().await.remove(&peer_id);
        tracing::debug!("Noise: removed session for peer {:02x?}", &peer_id);
    }

    /// Number of active sessions (handshake + established).
    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_a() -> PeerId { [0x01; PEER_ID_LEN] }
    fn peer_b() -> PeerId { [0x02; PEER_ID_LEN] }

    #[tokio::test]
    async fn full_handshake_and_encrypt_decrypt() {
        let (mgr_a, _) = NoiseSessionManager::generate().unwrap();
        let (mgr_b, _) = NoiseSessionManager::generate().unwrap();

        let msg1 = mgr_a.initiate_handshake(peer_b()).await.unwrap();
        let msg2 = mgr_b.receive_handshake_init(peer_a(), &msg1).await.unwrap();
        let msg3 = mgr_a.receive_handshake_response(peer_b(), &msg2).await.unwrap();
        mgr_b.finalize_responder_handshake(peer_a(), &msg3).await.unwrap();

        assert!(mgr_a.is_established(peer_b()).await);
        assert!(mgr_b.is_established(peer_a()).await);

        let pt = b"Hello AetherNet!";
        let ct = mgr_a.encrypt(peer_b(), pt).await.unwrap();
        let dec = mgr_b.decrypt(peer_a(), &ct).await.unwrap();
        assert_eq!(dec, pt);
    }

    #[tokio::test]
    async fn encrypt_without_session_returns_error() {
        let (mgr, _) = NoiseSessionManager::generate().unwrap();
        assert!(mgr.encrypt(peer_a(), b"test").await.is_err());
    }
}
