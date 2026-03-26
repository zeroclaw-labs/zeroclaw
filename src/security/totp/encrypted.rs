// Encrypted TOTP secret store.
//
// Secrets are encrypted with ChaCha20-Poly1305 AEAD using a key derived
// from ZeroClaw's master .secret_key via HKDF-SHA256 (Finding F12).
// Atomic writes via temp file + rename (D29).

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;

use super::types::UserTotpData;

const HKDF_SALT: &[u8] = b"zeroclaw-totp-v1";
const HKDF_INFO: &[u8] = b"totp-store-encryption";
const NONCE_SIZE: usize = 12;
const STORE_VERSION: u32 = 1;

/// Encrypted TOTP store. Manages per-user TOTP secrets on disk.
pub struct EncryptedTotpStore {
    store_path: PathBuf,
    derived_key: [u8; 32],
}

/// On-disk format: version (4 bytes LE) || nonce (12 bytes) || ciphertext
#[derive(serde::Serialize, serde::Deserialize)]
struct StoreContents {
    version: u32,
    users: HashMap<String, UserTotpData>,
}

impl EncryptedTotpStore {
    /// Create a new store, deriving the encryption key from the master key.
    pub fn new(store_path: impl Into<PathBuf>, master_key: &[u8]) -> Self {
        let derived_key = derive_key(master_key);
        Self {
            store_path: store_path.into(),
            derived_key,
        }
    }

    /// Load all user data from the encrypted store.
    pub fn load(&self) -> anyhow::Result<HashMap<String, UserTotpData>> {
        if !self.store_path.exists() {
            return Ok(HashMap::new());
        }

        let data = fs::read(&self.store_path)?;
        if data.len() < 4 + NONCE_SIZE + 1 {
            anyhow::bail!("store file too small");
        }

        // Read version
        let version = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if version != STORE_VERSION {
            anyhow::bail!("unsupported store version: {version}");
        }

        // Read nonce
        let nonce_bytes = &data[4..4 + NONCE_SIZE];
        let nonce = Nonce::from_slice(nonce_bytes);

        // Decrypt
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.derived_key));
        let plaintext = cipher
            .decrypt(nonce, &data[4 + NONCE_SIZE..])
            .map_err(|_| anyhow::anyhow!("decryption failed — wrong key or corrupted store"))?;

        let contents: StoreContents = serde_json::from_slice(&plaintext)?;
        Ok(contents.users)
    }

    /// Save all user data to the encrypted store (atomic write).
    pub fn save(&self, users: &HashMap<String, UserTotpData>) -> anyhow::Result<()> {
        let contents = StoreContents {
            version: STORE_VERSION,
            users: users.clone(),
        };
        let plaintext = serde_json::to_vec(&contents)?;

        // Generate fresh random nonce
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.derived_key));
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_slice())
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;

        // Build on-disk format: version || nonce || ciphertext
        let mut output = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        output.extend_from_slice(&STORE_VERSION.to_le_bytes());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);

        // Atomic write: temp file then rename (D29)
        let temp_path = self.store_path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(&output)?;
        file.sync_all()?;
        fs::rename(&temp_path, &self.store_path)?;

        // Set restrictive permissions (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.store_path, fs::Permissions::from_mode(0o600))?;
        }

        // Create backup
        let backup_path = self.store_path.with_extension("bak");
        let _ = fs::copy(&self.store_path, &backup_path);

        Ok(())
    }

    /// Get a single user's TOTP data.
    pub fn get_user(&self, user_id: &str) -> anyhow::Result<Option<UserTotpData>> {
        let users = self.load()?;
        Ok(users.get(user_id).cloned())
    }

    /// Upsert a single user's TOTP data.
    pub fn save_user(&self, user_id: &str, data: UserTotpData) -> anyhow::Result<()> {
        let mut users = self.load()?;
        users.insert(user_id.to_string(), data);
        self.save(&users)
    }

    /// Remove a user's TOTP data (revocation).
    pub fn remove_user(&self, user_id: &str) -> anyhow::Result<bool> {
        let mut users = self.load()?;
        let removed = users.remove(user_id).is_some();
        if removed {
            self.save(&users)?;
        }
        Ok(removed)
    }

    /// Get the derived signing key (for HMAC-signed gate decisions, F15).
    /// Uses a different HKDF info string to produce a separate key.
    pub fn signing_key(&self) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), &self.derived_key);
        let mut key = [0u8; 32];
        hk.expand(b"totp-gate-signing", &mut key)
            .expect("32 bytes is valid for SHA-256");
        key
    }
}

/// Derive the encryption key from the master key using HKDF-SHA256.
fn derive_key(master_key: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), master_key);
    let mut derived = [0u8; 32];
    hk.expand(HKDF_INFO, &mut derived)
        .expect("32 bytes is valid for SHA-256");
    derived
}

impl Drop for EncryptedTotpStore {
    fn drop(&mut self) {
        // Zeroize the derived key when the store is dropped
        self.derived_key.iter_mut().for_each(|b| *b = 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, EncryptedTotpStore) {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("totp.enc");
        let master_key = b"test-master-key-32-bytes-long!!!";
        let store = EncryptedTotpStore::new(store_path, master_key);
        (dir, store)
    }

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let (_dir, store) = test_store();

        let mut data = UserTotpData::default();
        data.user_id = "alice".to_string();
        data.secret_base32 = "JBSWY3DPEHPK3PXP".to_string();
        data.verified = true;

        store.save_user("alice", data.clone()).unwrap();

        let loaded = store.get_user("alice").unwrap().unwrap();
        assert_eq!(loaded.user_id, "alice");
        assert_eq!(loaded.secret_base32, "JBSWY3DPEHPK3PXP");
        assert!(loaded.verified);
    }

    #[test]
    fn wrong_key_fails() {
        let dir = TempDir::new().unwrap();
        let store_path = dir.path().join("totp.enc");

        // Write with key 1
        let store1 = EncryptedTotpStore::new(&store_path, b"key-one-32-bytes-long-padded!!!!");
        let mut data = UserTotpData::default();
        data.user_id = "bob".to_string();
        store1.save_user("bob", data).unwrap();

        // Read with key 2
        let store2 = EncryptedTotpStore::new(&store_path, b"key-two-32-bytes-long-padded!!!!");
        let result = store2.load();
        assert!(result.is_err());
    }

    #[test]
    fn empty_store_returns_empty() {
        let (_dir, store) = test_store();
        let users = store.load().unwrap();
        assert!(users.is_empty());
    }

    #[test]
    fn remove_user_works() {
        let (_dir, store) = test_store();

        let mut data = UserTotpData::default();
        data.user_id = "charlie".to_string();
        store.save_user("charlie", data).unwrap();

        assert!(store.get_user("charlie").unwrap().is_some());
        assert!(store.remove_user("charlie").unwrap());
        assert!(store.get_user("charlie").unwrap().is_none());
    }

    #[test]
    fn multiple_users() {
        let (_dir, store) = test_store();

        for name in &["alice", "bob", "charlie"] {
            let mut data = UserTotpData::default();
            data.user_id = name.to_string();
            data.secret_base32 = format!("SECRET_{}", name.to_uppercase());
            store.save_user(name, data).unwrap();
        }

        let users = store.load().unwrap();
        assert_eq!(users.len(), 3);
        assert_eq!(users["alice"].secret_base32, "SECRET_ALICE");
        assert_eq!(users["bob"].secret_base32, "SECRET_BOB");
    }

    #[test]
    fn signing_key_is_different_from_encryption_key() {
        let (_dir, store) = test_store();
        let signing = store.signing_key();
        // The signing key should not equal the raw derived encryption key
        assert_ne!(&signing[..], &store.derived_key[..]);
    }

    #[test]
    fn backup_created_on_save() {
        let (dir, store) = test_store();
        let mut data = UserTotpData::default();
        data.user_id = "test".to_string();
        store.save_user("test", data).unwrap();

        let backup_path = dir.path().join("totp.bak");
        assert!(backup_path.exists());
    }
}
