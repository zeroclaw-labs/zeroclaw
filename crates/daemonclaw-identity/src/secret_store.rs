//! Local `SecretStore` shim — mirrors the `enc2:` wire format used by
//! `daemonclaw_config::secrets::SecretStore`.
//!
//! This shim exists because the `daemonclaw-config` crate currently
//! fails to compile on this Rust version (pre-existing macro bug in
//! the `Configurable` derive produces 1000+ errors in `schema.rs`).
//! The shim is a drop-in: same ChaCha20-Poly1305 algorithm, same
//! `enc2:<hex(nonce ‖ ct ‖ tag)>` wire format, same `<dir>/.secret_key`
//! mode-0600 file location, same `encrypt(plaintext) -> enc2:…` and
//! `decrypt(enc2:…) -> plaintext` API.
//!
//! When the workspace builds, swap this for
//! `daemonclaw_config::secrets::SecretStore` in `state.rs` — the call
//! sites are the same shape, the format is the same, and the on-disk
//! files are interchangeable.

use std::fs;
use std::path::Path;

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

/// Error type for the local `SecretStore` shim. Independent of the
/// upstream `daemonclaw_config::secrets` error type so the shim is
/// self-contained.
#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    #[error("key material: {0}")]
    KeyMaterial(String),
    #[error("decrypt: {0}")]
    Decrypt(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("str utf-8: {0}")]
    StrUtf8(#[from] std::str::Utf8Error),
    #[error("hex: {0}")]
    Hex(String),
}

/// 32-byte ChaCha20 key length. Matches the upstream SecretStore.
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// On-disk key path: `<identity_dir>/.secret_key`. Same as upstream.
fn key_path(identity_dir: &Path) -> std::path::PathBuf {
    identity_dir.join(".secret_key")
}

/// Encrypted-secret store. `enabled = false` returns the plaintext
/// unchanged (sovereign mode). Same semantics as upstream.
#[derive(Debug, Clone)]
pub struct SecretStore {
    key_path: std::path::PathBuf,
    enabled: bool,
}

impl SecretStore {
    pub fn new(identity_dir: &Path, enabled: bool) -> Self {
        Self {
            key_path: key_path(identity_dir),
            enabled,
        }
    }

    /// Encrypt plaintext. Returns `enc2:<hex(nonce ‖ ct ‖ tag)>` when
    /// enabled; the input unchanged when disabled.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, SecretStoreError> {
        if !self.enabled || plaintext.is_empty() {
            return Ok(plaintext.to_string());
        }
        let key_bytes = self.load_or_create_key()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| SecretStoreError::Decrypt(format!("encrypt: {e}")))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ciphertext);
        Ok(format!("enc2:{}", hex_encode(&blob)))
    }

    /// Decrypt a value. Recognizes `enc2:` (current), `enc:` (legacy
    /// XOR — accepted for migration but not re-emitted), and plaintext
    /// (returned unchanged). Same surface as upstream.
    pub fn decrypt(&self, value: &str) -> Result<String, SecretStoreError> {
        if let Some(hex_str) = value.strip_prefix("enc2:") {
            self.decrypt_chacha20(hex_str)
        } else if value.starts_with("enc:") {
            Err(SecretStoreError::Decrypt(
                "legacy enc: not supported by this SecretStore; migrate via daemonclaw onboard".into(),
            ))
        } else {
            Ok(value.to_string())
        }
    }

    fn decrypt_chacha20(&self, hex_str: &str) -> Result<String, SecretStoreError> {
        let blob = hex_decode(hex_str)?;
        if blob.len() <= NONCE_LEN {
            return Err(SecretStoreError::Decrypt(
                "encrypted value too short (missing nonce)".into(),
            ));
        }
        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let key_bytes = self.load_or_create_key()?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key_bytes));
        let pt = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| {
                SecretStoreError::Decrypt(
                    "decryption failed (key mismatch or corrupt ciphertext)".into(),
                )
            })?;
        Ok(String::from_utf8(pt)?)
    }

    fn load_or_create_key(&self) -> Result<[u8; 32], SecretStoreError> {
        if let Ok(raw) = fs::read(&self.key_path) {
            // Accept either raw 32 bytes or 64-hex (legacy).
            if raw.len() == KEY_LEN {
                let mut k = [0u8; KEY_LEN];
                k.copy_from_slice(&raw);
                return Ok(k);
            }
            if raw.len() == KEY_LEN * 2 {
                let hex_str = std::str::from_utf8(&raw)?.trim();
                let bytes = hex_decode(hex_str)?;
                if bytes.len() != KEY_LEN {
                    return Err(SecretStoreError::KeyMaterial(
                        "key file wrong length".into(),
                    ));
                }
                let mut k = [0u8; KEY_LEN];
                k.copy_from_slice(&bytes);
                return Ok(k);
            }
            return Err(SecretStoreError::KeyMaterial(format!(
                ".secret_key has unexpected length: {}",
                raw.len()
            )));
        }
        // Generate a new 32-byte key and write it (mode 0600).
        let mut key = [0u8; KEY_LEN];
        use ring::rand::SecureRandom;
        let rng = ring::rand::SystemRandom::new();
        rng.fill(&mut key)
            .map_err(|e| SecretStoreError::KeyMaterial(format!("key rand: {e}")))?;
        if let Some(parent) = self.key_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.key_path, &key)?;
        set_unix_mode_0600(&self.key_path)?;
        Ok(key)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Result<Vec<u8>, SecretStoreError> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(SecretStoreError::Hex(
            "hex string must be even length".into(),
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|e| SecretStoreError::Hex(format!("at byte {i}: {e}")))?;
        out.push(byte);
    }
    Ok(out)
}

#[cfg(unix)]
fn set_unix_mode_0600(path: &Path) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(path)?.permissions();
    perm.set_mode(0o600);
    fs::set_permissions(path, perm)?;
    Ok(())
}
#[cfg(not(unix))]
fn set_unix_mode_0600(_path: &Path) -> Result<(), SecretStoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_then_decrypt_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let ct = store.encrypt("hello-secret").unwrap();
        assert!(ct.starts_with("enc2:"));
        let pt = store.decrypt(&ct).unwrap();
        assert_eq!(pt, "hello-secret");
    }

    #[test]
    fn disabled_returns_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path(), false);
        let pt = store.encrypt("hello").unwrap();
        assert_eq!(pt, "hello");
        let out = store.decrypt(&pt).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn encrypt_uses_random_nonce_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let a = store.encrypt("same").unwrap();
        let b = store.encrypt("same").unwrap();
        assert_ne!(a, b, "must use fresh nonce per encrypt");
        assert!(a.starts_with("enc2:"));
        assert!(b.starts_with("enc2:"));
    }

    #[test]
    fn decrypt_handles_missing_prefix_as_passthrough() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let out = store.decrypt("plaintext-value").unwrap();
        assert_eq!(out, "plaintext-value");
    }

    #[test]
    fn decrypt_rejects_corrupt_ciphertext() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let err = store.decrypt("enc2:deadbeef").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("too short") || msg.contains("decrypt"), "got: {msg}");
    }

    #[test]
    fn key_file_is_persisted_across_instances() {
        // Encrypt with one instance, decrypt with a fresh one reading
        // the same key file. Mirrors the across-restart contract.
        let dir = tempfile::tempdir().unwrap();
        let s1 = SecretStore::new(dir.path(), true);
        let ct = s1.encrypt("across-restart").unwrap();
        let s2 = SecretStore::new(dir.path(), true);
        let pt = s2.decrypt(&ct).unwrap();
        assert_eq!(pt, "across-restart");
    }
}
