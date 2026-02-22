pub mod key;

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::RngExt;
use std::collections::HashMap;

/// Versioned encryption service using XChaCha20-Poly1305.
/// Sealed format: "v{version}:{nonce_hex}:{ciphertext_hex}"
pub struct VaultService {
    current_version: u32,
    keys: HashMap<u32, [u8; 32]>,
}

impl VaultService {
    pub fn new(current_version: u32, keys: HashMap<u32, [u8; 32]>) -> Self {
        assert!(
            keys.contains_key(&current_version),
            "current key version not in keys map"
        );
        Self {
            current_version,
            keys,
        }
    }

    /// Encrypt plaintext with the current key version.
    /// Returns: "v{version}:{24_byte_nonce_hex}:{ciphertext_hex}"
    pub fn encrypt(&self, plaintext: &str) -> anyhow::Result<String> {
        let key = self.keys.get(&self.current_version).ok_or_else(|| {
            anyhow::anyhow!("current key version {} not found", self.current_version)
        })?;

        let cipher = XChaCha20Poly1305::new(key.into());
        let mut nonce_bytes = [0u8; 24];
        rand::rng().fill(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;

        Ok(format!(
            "v{}:{}:{}",
            self.current_version,
            hex::encode(nonce_bytes),
            hex::encode(ciphertext)
        ))
    }

    /// Decrypt a sealed string. Supports any key version present in the keys map.
    pub fn decrypt(&self, sealed: &str) -> anyhow::Result<String> {
        let parts: Vec<&str> = sealed.splitn(3, ':').collect();
        if parts.len() != 3 || !parts[0].starts_with('v') {
            anyhow::bail!("invalid sealed format: expected 'v<ver>:<nonce>:<ct>'");
        }

        let version: u32 = parts[0][1..].parse()?;
        let nonce_bytes = hex::decode(parts[1])?;
        let ciphertext = hex::decode(parts[2])?;

        if nonce_bytes.len() != 24 {
            anyhow::bail!(
                "invalid nonce length: expected 24 bytes, got {}",
                nonce_bytes.len()
            );
        }

        let key = self
            .keys
            .get(&version)
            .ok_or_else(|| anyhow::anyhow!("key version {} not found for decryption", version))?;

        let cipher = XChaCha20Poly1305::new(key.into());
        let nonce = XNonce::from_slice(&nonce_bytes);

        let plaintext = cipher.decrypt(nonce, ciphertext.as_ref()).map_err(|e| {
            anyhow::anyhow!("decryption failed (wrong key or corrupted data): {}", e)
        })?;

        Ok(String::from_utf8(plaintext)?)
    }

    /// Get current key version.
    pub fn current_version(&self) -> u32 {
        self.current_version
    }
}

#[cfg(test)]
impl VaultService {
    pub fn new_for_test() -> Self {
        let mut keys = std::collections::HashMap::new();
        let test_key = [0u8; 32]; // deterministic test key
        keys.insert(1, test_key);
        Self::new(1, keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vault() -> VaultService {
        let mut keys = HashMap::new();
        keys.insert(1, [0xAA; 32]);
        keys.insert(2, [0xBB; 32]);
        VaultService::new(2, keys)
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let vault = test_vault();
        let plaintext = "sk-secret-api-key-12345";
        let sealed = vault.encrypt(plaintext).unwrap();

        assert!(sealed.starts_with("v2:"));
        let decrypted = vault.decrypt(&sealed).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_old_version() {
        let mut keys = HashMap::new();
        keys.insert(1, [0xAA; 32]);
        let old_vault = VaultService::new(1, keys.clone());
        let sealed = old_vault.encrypt("old-secret").unwrap();

        // New vault with both versions can decrypt old sealed value
        keys.insert(2, [0xBB; 32]);
        let new_vault = VaultService::new(2, keys);
        let decrypted = new_vault.decrypt(&sealed).unwrap();
        assert_eq!(decrypted, "old-secret");
    }

    #[test]
    fn test_decrypt_wrong_key_fails() {
        let vault = test_vault();
        let sealed = vault.encrypt("secret").unwrap();

        // Vault with different key for same version must fail
        let mut bad_keys = HashMap::new();
        bad_keys.insert(2, [0xCC; 32]);
        let bad_vault = VaultService::new(2, bad_keys);
        assert!(bad_vault.decrypt(&sealed).is_err());
    }

    #[test]
    fn test_invalid_format() {
        let vault = test_vault();
        assert!(vault.decrypt("invalid").is_err());
        // 12 hex bytes = 6 raw bytes, not 24 — should fail nonce length check
        assert!(vault.decrypt("v1:aabbccddeeff001122334455:data").is_err());
        assert!(vault.decrypt("nope:aabb:ccdd").is_err());
    }

    #[test]
    fn test_empty_plaintext() {
        let vault = test_vault();
        let sealed = vault.encrypt("").unwrap();
        let decrypted = vault.decrypt(&sealed).unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn test_unicode_plaintext() {
        let vault = test_vault();
        let sealed = vault.encrypt("héllo wörld 🦀").unwrap();
        let decrypted = vault.decrypt(&sealed).unwrap();
        assert_eq!(decrypted, "héllo wörld 🦀");
    }

    #[test]
    fn test_nonce_is_unique_per_encrypt() {
        let vault = test_vault();
        let sealed1 = vault.encrypt("same-data").unwrap();
        let sealed2 = vault.encrypt("same-data").unwrap();
        // Ciphertexts differ due to random nonce
        assert_ne!(sealed1, sealed2);
    }

    #[test]
    fn test_current_version_accessor() {
        let vault = test_vault();
        assert_eq!(vault.current_version(), 2);
    }
}

#[cfg(test)]
mod key_tests {
    use super::key;
    use std::path::PathBuf;

    fn temp_key_path() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("zcplatform-test-{}.key", uuid::Uuid::new_v4()));
        path
    }

    #[test]
    fn test_generate_and_load_key() {
        let path = temp_key_path();

        // Generate first key
        let v1 = key::generate_key(&path).unwrap();
        assert_eq!(v1, 1);

        // Load and verify
        let (current, keys) = key::load_keys(&path).unwrap();
        assert_eq!(current, 1);
        assert_eq!(keys.len(), 1);
        assert!(keys.contains_key(&1));

        // Generate second key (rotated)
        let v2 = key::generate_key(&path).unwrap();
        assert_eq!(v2, 2);

        let (current, keys) = key::load_keys(&path).unwrap();
        assert_eq!(current, 2);
        assert_eq!(keys.len(), 2);

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_nonexistent_fails() {
        let path = PathBuf::from("/tmp/nonexistent-key-file-zcplatform-test-never-exists.key");
        assert!(key::load_keys(&path).is_err());
    }

    #[test]
    fn test_key_file_permissions() {
        // Only meaningful on Unix; skip silently on other platforms
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = temp_key_path();
            key::generate_key(&path).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "key file must be owner-read/write only");
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn test_generated_keys_are_usable_in_vault() {
        let path = temp_key_path();
        key::generate_key(&path).unwrap();
        key::generate_key(&path).unwrap();

        let (current, keys) = key::load_keys(&path).unwrap();
        let vault = super::VaultService::new(current, keys);
        let sealed = vault.encrypt("vault-key-integration").unwrap();
        let decrypted = vault.decrypt(&sealed).unwrap();
        assert_eq!(decrypted, "vault-key-integration");

        let _ = std::fs::remove_file(&path);
    }
}
