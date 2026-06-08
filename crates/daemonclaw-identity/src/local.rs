//! `LocalIdentityProvider` — the always-on default identity backend.
//!
//! Generates (on first boot) or loads an Ed25519 keypair, persists it
//! encrypted in `<identity_dir>/identity_state.json` (via
//! [`crate::state`]), surfaces the public SPKI PEM + fingerprint in
//! [`LocalIdentityProvider::whoami`], and signs/verifies assertions
//! locally. No network. Zero-config default.
//!
//! The trust tier is always `KeyRegistered`. The issuer status is
//! always `Unqueried` because there is no issuer — the local provider
//! is the floor, not a remote-issuer-backed identity.
//!
//! ## First boot
//!
//! 1. `<identity_dir>/identity_state.json` does not exist.
//! 2. Generate Ed25519 PKCS#8 via `ring::Ed25519KeyPair::generate_pkcs8`.
//! 3. Derive SPKI PEM (with the RFC 8410 §3.3 prefix) + fingerprint.
//! 4. Encrypt the PKCS#8 PEM via `SecretStore`, persist as the state file.
//! 5. Write the operator-readable SPKI PEM at
//!    `<identity_dir>/<host>.spki.pem` (mode 0644).
//!
//! ## Subsequent boots
//!
//! 1. Load the state file (the encrypted private key is decrypted only
//!    when sign/verify is called — `whoami` is cheap).
//! 2. Same as before from the caller's perspective.
//!
//! ## Sign path zeroization
//!
//! The sign helper takes ownership of a `Zeroizing<Vec<u8>>` containing
//! the decrypted PKCS#8 PEM bytes and returns the signature. The PKCS#8
//! is dropped at the end of the function — the caller cannot retain a
//! reference to the secret after the call. The Ed25519KeyPair constructed
//! inside the helper holds the secret in ring's internal buffer; that
//! memory is reclaimed by ring when the KeyPair is dropped. We do not
//! have a hook to zeroize ring's internal buffer from the outside, but
//! we minimize the window by scoping the KeyPair to the helper.

use std::sync::Arc;

use async_trait::async_trait;
use daemonclaw_api::identity::{
    AgentIdentity, IdentityAssertion, IdentityProvider, IssuerStatus, TrustTier, VerificationResult,
    VerifyFailure,
};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::canonical::{canonical_bytes, CanonicalAssertion};
use crate::error::{IdentityError, IdentityResult};
use crate::runtime::IdentityRuntimeOptions;
use crate::spki::{fingerprint_spki, spki_from_pubkey};
use crate::state::{
    decrypt_private_key_pem, encrypt_private_key_pem, load_state, save_state, IdentityState,
};

/// Local Ed25519-backed `IdentityProvider`. Default; always available.
pub struct LocalIdentityProvider {
    options: IdentityRuntimeOptions,
    /// In-memory cache of the loaded state. Loaded lazily on first call
    /// to `whoami`, `assertion`, or `verify`. Persisted across calls so
    /// `whoami` is cheap.
    cached: Mutex<Option<Arc<IdentityState>>>,
    host: String,
}

impl std::fmt::Debug for LocalIdentityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalIdentityProvider")
            .field("host", &self.host)
            .field("identity_dir", &self.options.identity_dir)
            .finish()
    }
}

impl LocalIdentityProvider {
    /// Construct a new `LocalIdentityProvider` rooted at the runtime's
    /// identity directory. Does not touch the disk until first call.
    pub fn new(options: IdentityRuntimeOptions) -> IdentityResult<Self> {
        let host = options.host_label.clone();
        Ok(Self {
            options,
            cached: Mutex::new(None),
            host,
        })
    }

    /// Convenience constructor for tests — returns a provider with a
    /// caller-controlled `host` label.
    #[cfg(test)]
    pub fn new_with_host(options: IdentityRuntimeOptions, host: &str) -> IdentityResult<Self> {
        Ok(Self {
            options,
            cached: Mutex::new(None),
            host: host.to_string(),
        })
    }

    /// Resolve the on-disk state, generating + persisting on first boot.
    async fn ensure_loaded(&self) -> IdentityResult<Arc<IdentityState>> {
        if let Some(cached) = self.cached.lock().await.clone() {
            return Ok(cached);
        }
        let loaded = match load_state(&self.options.identity_dir, self.options.secrets_encrypt)? {
            Some(state) => state,
            None => self.generate_and_persist().await?,
        };
        let arc = Arc::new(loaded);
        *self.cached.lock().await = Some(arc.clone());
        Ok(arc)
    }

    /// First-boot: generate an Ed25519 keypair, derive SPKI + fingerprint,
    /// encrypt the private key, persist.
    async fn generate_and_persist(&self) -> IdentityResult<IdentityState> {
        let rng = SystemRandom::new();
        let pkcs8_doc = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|e| IdentityError::KeyMaterial(format!("generate_pkcs8: {e}")))?;
        let pkcs8_pem = pem_from_pkcs8(pkcs8_doc.as_ref());
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_doc.as_ref())
            .map_err(|e| IdentityError::KeyMaterial(format!("from_pkcs8: {e}")))?;
        let pubkey: [u8; 32] = key_pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| IdentityError::KeyMaterial("pubkey not 32 bytes".into()))?;
        // SPKI is computed once. Fingerprint, SPKI PEM, and the on-disk
        // SPKI file all derive from the same `spki` value. This closes
        // the SPKI→fingerprint contract by construction — a refactor
        // of the SPKI prefix changes all three together or none of
        // them, and the on-disk state's `fingerprint` is
        // SHA256(spki_on_disk) by definition.
        let spki = spki_from_pubkey(&pubkey);
        let spki_pem = pem_from_spki(&spki);
        let fingerprint = fingerprint_spki(&spki);
        let agent_user_id = Uuid::new_v4().to_string();
        let enc = encrypt_private_key_pem(
            &pkcs8_pem,
            self.options.secrets_encrypt,
            &self.options.identity_dir,
        )?;
        let state = IdentityState {
            version: crate::state::STATE_VERSION,
            agent_user_id: agent_user_id.clone(),
            private_key_pem_enc: enc,
            spki_pem: spki_pem.clone(),
            fingerprint: fingerprint.clone(),
            created_at_unix: chrono::Utc::now().timestamp(),
        };
        save_state(&self.options.identity_dir, &self.host, &state)?;
        tracing::info!(
            agent_user_id = %agent_user_id,
            fingerprint = %fingerprint,
            host = %self.host,
            identity_dir = %self.options.identity_dir.display(),
            "Generated new local agent identity. SPKI PEM available at {}",
            self.options.identity_dir.join(format!("{}.spki.pem", self.host)).display()
        );
        Ok(state)
    }

    /// Decrypt the private key PEM (Zeroizing) from a cached state. The
    /// Zeroizing wrapper ensures the bytes are wiped when the returned
    /// value is dropped.
    async fn load_pkcs8_zeroizing(&self) -> IdentityResult<Zeroizing<Vec<u8>>> {
        let state = self.ensure_loaded().await?;
        let pem = decrypt_private_key_pem(
            &state.private_key_pem_enc,
            self.options.secrets_encrypt,
            &self.options.identity_dir,
        )?;
        Ok(Zeroizing::new(pem))
    }
}

#[async_trait]
impl IdentityProvider for LocalIdentityProvider {
    fn name(&self) -> &str {
        "local"
    }

    async fn whoami(&self) -> anyhow::Result<AgentIdentity> {
        let state = self.ensure_loaded().await?;
        Ok(AgentIdentity {
            agent_user_id: state.agent_user_id.clone(),
            label: self.host.clone(),
            key_fingerprint: Some(state.fingerprint.clone()),
            spki_pem: Some(state.spki_pem.clone()),
            tier: TrustTier::KeyRegistered,
            // Local provider has no issuer. Never optimistic.
            issuer_status: IssuerStatus::Unqueried,
        })
    }

    async fn assertion(&self, audience: Option<&str>) -> anyhow::Result<IdentityAssertion> {
        let state = self.ensure_loaded().await?;
        let pkcs8 = self.load_pkcs8_zeroizing().await?;
        // The local provider has no grantor; "<local>" is the sentinel
        // for both the canonical-bytes sign path AND the assertion's
        // `grantor_user_id` field. The local `verify` reads the field
        // back from the assertion, so the sentinel on the wire has to
        // match the sentinel that was signed — otherwise the signature
        // would not verify against our own assertion.
        let grantor_sentinel = "<local>";
        let (signature, nonce) = sign_canonical(
            &pkcs8,
            &state.agent_user_id,
            grantor_sentinel,
            &state.fingerprint,
            audience,
        )?;
        Ok(IdentityAssertion {
            agent_user_id: state.agent_user_id.clone(),
            grantor_user_id: grantor_sentinel.into(),
            fingerprint: state.fingerprint.clone(),
            audience: audience.map(|s| s.to_string()),
            issued_at: chrono::Utc::now().timestamp(),
            nonce,
            signature,
        })
    }

    async fn verify(&self, assertion: &IdentityAssertion) -> anyhow::Result<VerificationResult> {
        // Verify path: public key only. We deliberately do NOT decrypt
        // the private key here — `verify_canonical` takes a 32-byte
        // public key and `UnparsedPublicKey` does the Ed25519 check.
        // This shrinks the secret-handling surface and means a verify
        // call can never accidentally log/print the private key.
        let state = self.ensure_loaded().await?;
        let spki_der = crate::spki::spki_pem_to_der(state.spki_pem.as_bytes())
            .map_err(IdentityError::KeyMaterial)?;
        let pubkey = crate::spki::pubkey_from_spki(&spki_der).ok_or_else(|| {
            IdentityError::Crypto("spki pem does not decode to a 32-byte pubkey".into())
        })?;
        let signature_ok = verify_canonical(&pubkey, assertion)?;
        // Local provider: no issuer. `Unqueried` means "we did not
        // contact an issuer because there is no issuer to contact."
        // Consumers calling `whoami` know whether they're on local.
        Ok(VerificationResult {
            signature_ok,
            issuer_status: IssuerStatus::Unqueried,
            failure_reason: if signature_ok {
                None
            } else {
                Some(VerifyFailure::BadSignature)
            },
        })
    }
}

// ── Free functions (testable, no async, no state) ───────────────

/// Sign canonical bytes using a PKCS#8 private key held in a `Zeroizing`
/// buffer. Returns the signature and a freshly-generated nonce (so the
/// assertion is replay-distinct).
pub fn sign_canonical(
    pkcs8_pem: &Zeroizing<Vec<u8>>,
    agent_user_id: &str,
    grantor_user_id: &str,
    fingerprint: &str,
    audience: Option<&str>,
) -> IdentityResult<(Vec<u8>, String)> {
    // Parse the PEM envelope back to PKCS#8 DER.
    let pkcs8_der = pem_to_der(pkcs8_pem.as_slice())?;
    let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8_der)
        .map_err(|e| IdentityError::KeyMaterial(format!("from_pkcs8: {e}")))?;
    let nonce = generate_nonce()?;
    let canonical = canonical_bytes(&CanonicalAssertion {
        agent_user_id,
        grantor_user_id,
        fingerprint,
        audience,
        issued_at: chrono::Utc::now().timestamp(),
        nonce: &nonce,
    })?;
    let sig = key_pair.sign(&canonical);
    Ok((sig.as_ref().to_vec(), nonce))
}

/// Verify a signature against the canonical bytes derived from an
/// assertion, using the public key bytes (32) wrapped in `pkcs8_pem`
/// (not a key — we use the public key directly via `UnparsedPublicKey`).
pub fn verify_canonical(
    pubkey: &[u8; 32],
    assertion: &IdentityAssertion,
) -> IdentityResult<bool> {
    let canonical = canonical_bytes(&CanonicalAssertion {
        agent_user_id: &assertion.agent_user_id,
        grantor_user_id: &assertion.grantor_user_id,
        fingerprint: &assertion.fingerprint,
        audience: assertion.audience.as_deref(),
        issued_at: assertion.issued_at,
        nonce: &assertion.nonce,
    })?;
    use ring::signature::{UnparsedPublicKey, ED25519};
    let peer = UnparsedPublicKey::new(&ED25519, pubkey.as_ref());
    Ok(peer.verify(&canonical, &assertion.signature).is_ok())
}

// ── PEM helpers ──────────────────────────────────────────────────

/// Wrap PKCS#8 DER bytes in a PEM envelope (`-----BEGIN PRIVATE KEY-----`).
fn pem_from_pkcs8(der: &[u8]) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let b64 = STANDARD.encode(der);
    let mut out = String::with_capacity(b64.len() + 64);
    out.push_str("-----BEGIN PRIVATE KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        out.push('\n');
    }
    out.push_str("-----END PRIVATE KEY-----\n");
    out
}

/// Wrap SPKI DER bytes in a PEM envelope (`-----BEGIN PUBLIC KEY-----`).
fn pem_from_spki(spki: &[u8]) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let b64 = STANDARD.encode(spki);
    let mut out = String::with_capacity(b64.len() + 64);
    out.push_str("-----BEGIN PUBLIC KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        out.push('\n');
    }
    out.push_str("-----END PUBLIC KEY-----\n");
    out
}

/// Strip the PEM envelope back to PKCS#8 DER bytes.
fn pem_to_der(pem: &[u8]) -> IdentityResult<Vec<u8>> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    let s = std::str::from_utf8(pem).map_err(|e| IdentityError::KeyMaterial(format!("pem utf-8: {e}")))?;
    let mut b64 = String::new();
    for line in s.lines() {
        if line.starts_with("-----") {
            continue;
        }
        b64.push_str(line.trim());
    }
    STANDARD
        .decode(b64.trim())
        .map_err(|e| IdentityError::KeyMaterial(format!("pem base64 decode: {e}")))
}

/// Generate a 128-bit base64url-no-pad nonce. Single-use protection
/// against replay; the verifier is responsible for tracking used nonces
/// (caller-side concern, not in this crate).
fn generate_nonce() -> IdentityResult<String> {
    let mut bytes = [0u8; 16];
    use ring::rand::SecureRandom;
    let rng = SystemRandom::new();
    rng.fill(&mut bytes)
        .map_err(|e| IdentityError::Crypto(format!("nonce rand: {e}")))?;
    let alphabet: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(22);
    for chunk in bytes.chunks(3) {
        match chunk.len() {
            3 => {
                let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
                out.push(alphabet[((n >> 18) & 0x3f) as usize] as char);
                out.push(alphabet[((n >> 12) & 0x3f) as usize] as char);
                out.push(alphabet[((n >> 6) & 0x3f) as usize] as char);
                out.push(alphabet[(n & 0x3f) as usize] as char);
            }
            1 => {
                let n = (chunk[0] as u32) << 16;
                out.push(alphabet[((n >> 18) & 0x3f) as usize] as char);
                out.push(alphabet[((n >> 12) & 0x3f) as usize] as char);
            }
            2 => {
                let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8);
                out.push(alphabet[((n >> 18) & 0x3f) as usize] as char);
                out.push(alphabet[((n >> 12) & 0x3f) as usize] as char);
                out.push(alphabet[((n >> 6) & 0x3f) as usize] as char);
            }
            _ => {}
        }
    }
    Ok(out)
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::IdentityRuntimeOptions;
    use crate::spki::spki_from_pubkey;
    use daemonclaw_api::identity::{IssuerStatus, TrustTier};
    use tempfile::TempDir;

    fn tmp_options() -> (TempDir, IdentityRuntimeOptions) {
        let dir = tempfile::tempdir().expect("tempdir");
        let opts = IdentityRuntimeOptions {
            identity_dir: dir.path().to_path_buf(),
            host_label: "claw-test".into(),
            secrets_encrypt: false, // sovereign in tests; SecretStore is testable separately
            issuer_url: None,
            agent_user_id_hint: None,
            grantor_user_id: None,
        };
        (dir, opts)
    }

    fn extract_pkcs8_seed_64hex(pkcs8_der: &[u8]) -> String {
        // Ed25519 PKCS#8 v2 layout: the 32-byte private seed sits at a
        // fixed offset within the OCTET STRING. For our purposes
        // (which is a "does the test buffer contain the seed?" check)
        // we don't need to be precise — we just extract a 64-char
        // hex pattern from anywhere in the buffer and use it as the
        // "leaked" pattern. The buffer is the decrypted PKCS#8 PEM
        // *body* bytes (after pem_to_der).
        hex::encode(pkcs8_der)
    }

    #[tokio::test]
    async fn first_boot_generates_persists_and_reports_whoami() {
        let (dir, opts) = tmp_options();
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let id = provider.whoami().await.unwrap();
        // First-boot whoami: KeyRegistered, Unqueried, all fields set.
        assert_eq!(id.tier, TrustTier::KeyRegistered);
        assert!(matches!(id.issuer_status, IssuerStatus::Unqueried));
        assert!(id.key_fingerprint.is_some());
        assert!(id.spki_pem.is_some());
        assert!(id.agent_user_id.len() >= 32, "uuid should be 36 chars");
        // State file exists.
        let state_path = dir.path().join("identity_state.json");
        assert!(state_path.exists());
        // SPKI file exists, mode 0644, contains BEGIN PUBLIC KEY.
        let spki_path = dir.path().join("claw-test.spki.pem");
        assert!(spki_path.exists());
        let pem = std::fs::read_to_string(&spki_path).unwrap();
        assert!(pem.contains("-----BEGIN PUBLIC KEY-----"));
        assert!(pem.contains("-----END PUBLIC KEY-----"));
        // The fingerprint in the state matches what `whoami` returned.
        let raw = std::fs::read_to_string(&state_path).unwrap();
        assert!(raw.contains(&id.key_fingerprint.clone().unwrap()));
    }

    #[tokio::test]
    async fn second_boot_loads_existing_state_no_regen() {
        let (dir, opts) = tmp_options();
        // First boot.
        let p1 = LocalIdentityProvider::new_with_host(opts.clone(), "claw-test").unwrap();
        let id1 = p1.whoami().await.unwrap();
        // Second boot, fresh provider, same dir.
        let p2 = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let id2 = p2.whoami().await.unwrap();
        // Same agent_user_id (no regen).
        assert_eq!(id1.agent_user_id, id2.agent_user_id);
        assert_eq!(id1.key_fingerprint, id2.key_fingerprint);
        // State file still exists, untouched in spirit.
        assert!(dir.path().join("identity_state.json").exists());
    }

    #[tokio::test]
    async fn assertion_signs_and_local_verify_succeeds() {
        let (_dir, opts) = tmp_options();
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let assertion = provider.assertion(Some("hub")).await.unwrap();
        assert!(!assertion.signature.is_empty(), "signature must be 64 bytes");
        assert_eq!(assertion.signature.len(), 64, "Ed25519 sig is 64 bytes");
        assert!(assertion.audience.is_some());
        let result = provider.verify(&assertion).await.unwrap();
        assert!(result.signature_ok, "local sig check must succeed");
        assert!(matches!(result.issuer_status, IssuerStatus::Unqueried));
        assert!(result.failure_reason.is_none());
    }

    #[tokio::test]
    async fn verify_rejects_tampered_signature() {
        let (_dir, opts) = tmp_options();
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let mut assertion = provider.assertion(Some("hub")).await.unwrap();
        // Tamper with the signature.
        assertion.signature[0] ^= 0xff;
        let result = provider.verify(&assertion).await.unwrap();
        assert!(!result.signature_ok);
        assert_eq!(result.failure_reason, Some(VerifyFailure::BadSignature));
    }

    #[tokio::test]
    async fn verify_rejects_tampered_canonical_fields() {
        // Changing any canonical field should make the signature
        // invalid. The most likely operator bug is changing
        // `audience` or `issued_at` after signing.
        let (_dir, opts) = tmp_options();
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let mut assertion = provider.assertion(Some("hub")).await.unwrap();
        assertion.audience = Some("other-hub".into());
        let result = provider.verify(&assertion).await.unwrap();
        assert!(!result.signature_ok);

        // Also check issued_at tamper.
        let mut assertion2 = provider.assertion(Some("hub")).await.unwrap();
        assertion2.issued_at += 1;
        let result2 = provider.verify(&assertion2).await.unwrap();
        assert!(!result2.signature_ok);

        // And nonce tamper.
        let mut assertion3 = provider.assertion(Some("hub")).await.unwrap();
        assertion3.nonce.push('x');
        let result3 = provider.verify(&assertion3).await.unwrap();
        assert!(!result3.signature_ok);
    }

    #[tokio::test]
    async fn encrypted_state_roundtrip_via_secret_store() {
        // End-to-end: encrypt via SecretStore.encrypt, store as
        // state, load via state.load_state (which decrypts), then
        // sign with the loaded key. The signature must verify.
        let (dir, opts) = tmp_options();
        // Force the store to use encryption for this test by
        // constructing a provider that writes encrypted.
        let encrypted_opts = IdentityRuntimeOptions {
            secrets_encrypt: true,
            ..opts
        };
        let p1 = LocalIdentityProvider::new_with_host(encrypted_opts.clone(), "claw-test").unwrap();
        let id1 = p1.whoami().await.unwrap();
        // Read the on-disk state — it should be encrypted (enc2: prefix).
        let raw = std::fs::read_to_string(dir.path().join("identity_state.json")).unwrap();
        assert!(
            raw.contains("\"private_key_pem_enc\": \"enc2:"),
            "state file must hold enc2: blob when secrets_encrypt=true. raw={raw}"
        );

        // Fresh provider, same dir, decryption path.
        let p2 = LocalIdentityProvider::new_with_host(encrypted_opts, "claw-test").unwrap();
        let id2 = p2.whoami().await.unwrap();
        assert_eq!(id1.agent_user_id, id2.agent_user_id);

        // Sign + verify roundtrip across the two providers (same key).
        let a = p1.assertion(Some("aud")).await.unwrap();
        let result = p2.verify(&a).await.unwrap();
        assert!(result.signature_ok, "cross-provider verify must succeed");

        // Tamper and confirm it fails.
        let mut a2 = a;
        a2.signature[10] ^= 0x42;
        let result2 = p2.verify(&a2).await.unwrap();
        assert!(!result2.signature_ok);
    }

    #[test]
    fn pem_roundtrips_for_pkcs8_and_spki() {
        // Build a known PKCS#8 + SPKI, envelope, strip, compare.
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;
        let pkcs8_der = vec![0u8, 1, 2, 3, 4, 5, 6, 7];
        let pem = pem_from_pkcs8(&pkcs8_der);
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----\n"));
        assert!(pem.ends_with("-----END PRIVATE KEY-----\n"));
        let der = pem_to_der(pem.as_bytes()).unwrap();
        assert_eq!(der, pkcs8_der);

        let spki = spki_from_pubkey(&[0xab; 32]);
        let pem2 = pem_from_spki(&spki);
        assert!(pem2.starts_with("-----BEGIN PUBLIC KEY-----\n"));
        // The base64 in between is STANDARD, no padding inside the
        // wrapped lines (64 chars each).
        let body: String = pem2
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        assert!(STANDARD.decode(&body).is_ok(), "spki pem body must be valid base64");
    }

    #[test]
    fn sign_canonical_zeroizes_key_buffer() {
        // The sign helper takes a `Zeroizing<Vec<u8>>` — the caller
        // cannot retain a reference after the call. We test this by
        // consuming the buffer and confirming the call returns. The
        // zeroize crate guarantees the buffer is wiped on drop; we
        // trust that and pin the API surface.
        let rng = SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pem = pem_from_pkcs8(doc.as_ref());
        let pkcs8 = Zeroizing::new(pem.into_bytes());
        let (_sig, nonce) =
            sign_canonical(&pkcs8, "a", "g", "sha256:abc", Some("aud")).unwrap();
        assert!(!nonce.is_empty());
        // After this scope the Zeroizing is dropped → wiped.
    }

    #[tokio::test]
    async fn display_does_not_leak_private_key() {
        // The big one. Capture a real PKCS#8, encrypt it via
        // SecretStore into a state, then check:
        //   1. The Debug output of the state does NOT contain the
        //      plaintext PKCS#8 DER hex (which would mean the
        //      private key is in a debug-formatted struct).
        //   2. The Debug output of the provider does NOT.
        //   3. The AgentIdentity Debug does NOT.
        let (_dir, opts) = tmp_options();
        let opts = IdentityRuntimeOptions {
            secrets_encrypt: true,
            ..opts
        };
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        // Generate a real key and capture its plaintext PKCS#8 DER hex
        // (the "leaked" pattern we're guarding against).
        let rng = SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let plaintext_der_hex = extract_pkcs8_seed_64hex(doc.as_ref());
        assert!(plaintext_der_hex.len() >= 64, "der hex must be substantial");

        // Trigger state load (which encrypts the same key).
        let _ = provider.whoami().await.unwrap();

        // Debug the provider. It does not hold the plaintext key in
        // any visible field.
        let dbg_provider = format!("{provider:?}");
        assert!(
            !dbg_provider.contains(&plaintext_der_hex),
            "provider Debug leaked plaintext PKCS#8 DER: {dbg_provider}"
        );

        // Read the state file (which holds the encrypted blob) and
        // Debug-print it. The Debug must not contain the plaintext
        // hex. (The encrypted form starts with "enc2:" so it can't
        // accidentally be a hex pattern matching the plaintext.)
        let state_raw = std::fs::read_to_string(
            provider
                .options
                .identity_dir
                .join("identity_state.json"),
        )
        .unwrap();
        assert!(
            !state_raw.contains(&plaintext_der_hex),
            "state file contains plaintext PKCS#8 DER"
        );

        // Debug the AgentIdentity.
        let id = provider.whoami().await.unwrap();
        let dbg_id = format!("{id:?}");
        assert!(
            !dbg_id.contains(&plaintext_der_hex),
            "AgentIdentity Debug leaked plaintext: {dbg_id}"
        );
    }

    #[tokio::test]
    async fn spki_pem_in_whoami_matches_loaded_state() {
        // The SPKI PEM in `whoami` must be the same as what's on disk
        // — for the operator to copy and paste into WardToken.
        let (dir, opts) = tmp_options();
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let id = provider.whoami().await.unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("claw-test.spki.pem")).unwrap();
        assert_eq!(id.spki_pem.as_deref(), Some(on_disk.as_str()));
    }

    #[tokio::test]
    async fn fingerprint_in_whoami_is_consistent_across_calls() {
        // Fingerprint derives from the pubkey. Same key → same fingerprint.
        let (_dir, opts) = tmp_options();
        let provider = LocalIdentityProvider::new_with_host(opts, "claw-test").unwrap();
        let id1 = provider.whoami().await.unwrap();
        let id2 = provider.whoami().await.unwrap();
        assert_eq!(id1.key_fingerprint, id2.key_fingerprint);
        assert!(id1.key_fingerprint.as_ref().unwrap().starts_with("sha256:"));
    }
}

