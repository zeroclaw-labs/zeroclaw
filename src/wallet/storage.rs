//! Encrypted wallet storage — reuses the existing `SecretStore` for key encryption.

use super::keypair::{WalletAddress, WalletKeypair};
use crate::security::secrets::SecretStore;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Persists and loads wallet private keys using encrypted storage.
pub struct WalletStore {
    wallet_path: PathBuf,
    secret_store: SecretStore,
}

impl WalletStore {
    /// Create a wallet store rooted at the given directory.
    ///
    /// - `wallet_dir` — directory containing `wallet.key` (encrypted private key)
    /// - `zeroclaw_dir` — root directory for `SecretStore` key file
    pub fn new(wallet_dir: &Path, zeroclaw_dir: &Path) -> Self {
        Self {
            wallet_path: wallet_dir.join("wallet.key"),
            secret_store: SecretStore::new(zeroclaw_dir, true),
        }
    }

    /// Check if a wallet file exists on disk.
    pub fn exists(&self) -> bool {
        self.wallet_path.exists()
    }

    /// Save a keypair to encrypted storage.
    pub fn save(&self, keypair: &WalletKeypair) -> Result<()> {
        let hex_key = keypair.private_key_hex();
        let encrypted = self
            .secret_store
            .encrypt(&hex_key)
            .context("Failed to encrypt wallet private key")?;

        if let Some(parent) = self.wallet_path.parent() {
            fs::create_dir_all(parent).context("Failed to create wallet directory")?;
        }

        fs::write(&self.wallet_path, &encrypted).context("Failed to write wallet file")?;

        // Set restrictive permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.wallet_path, fs::Permissions::from_mode(0o600))
                .context("Failed to set wallet file permissions")?;
        }

        tracing::info!(
            address = %keypair.address(),
            path = %self.wallet_path.display(),
            "Wallet saved (encrypted)"
        );
        Ok(())
    }

    /// Load a keypair from encrypted storage.
    pub fn load(&self) -> Result<WalletKeypair> {
        let encrypted =
            fs::read_to_string(&self.wallet_path).context("Failed to read wallet file")?;
        let hex_key = self
            .secret_store
            .decrypt(encrypted.trim())
            .context("Failed to decrypt wallet private key")?;
        WalletKeypair::from_hex(&hex_key)
    }

    /// Load or generate a wallet. Returns `(keypair, was_generated)`.
    pub fn load_or_generate(&self) -> Result<(WalletKeypair, bool)> {
        if self.exists() {
            let kp = self.load()?;
            Ok((kp, false))
        } else {
            let kp = WalletKeypair::generate();
            self.save(&kp)?;
            Ok((kp, true))
        }
    }

    /// Get the address without loading the full keypair (reads + decrypts).
    pub fn address(&self) -> Result<WalletAddress> {
        let kp = self.load()?;
        Ok(kp.address())
    }

    /// Path to the wallet file (for display).
    pub fn wallet_path(&self) -> &Path {
        &self.wallet_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store(tmp: &TempDir) -> WalletStore {
        let wallet_dir = tmp.path().join("wallet");
        let zeroclaw_dir = tmp.path().join("zeroclaw");
        WalletStore::new(&wallet_dir, &zeroclaw_dir)
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let kp = WalletKeypair::generate();
        let addr = kp.address();
        store.save(&kp).unwrap();

        assert!(store.exists());

        let loaded = store.load().unwrap();
        assert_eq!(loaded.address(), addr);
    }

    #[test]
    fn load_nonexistent_fails() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);
        assert!(!store.exists());
        assert!(store.load().is_err());
    }

    #[test]
    fn load_or_generate_creates_new() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        assert!(!store.exists());
        let (kp, was_generated) = store.load_or_generate().unwrap();
        assert!(was_generated);
        assert!(store.exists());

        // Second call loads existing
        let (kp2, was_generated2) = store.load_or_generate().unwrap();
        assert!(!was_generated2);
        assert_eq!(kp.address(), kp2.address());
    }

    #[test]
    fn stored_key_is_encrypted() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let kp = WalletKeypair::generate();
        let raw_hex = kp.private_key_hex();
        store.save(&kp).unwrap();

        let on_disk = fs::read_to_string(store.wallet_path()).unwrap();
        assert!(
            on_disk.starts_with("enc2:"),
            "Wallet file must be encrypted"
        );
        assert!(
            !on_disk.contains(&raw_hex),
            "Wallet file must not contain plaintext key"
        );
    }

    #[test]
    fn address_reads_without_full_load() {
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let kp = WalletKeypair::generate();
        let expected_addr = kp.address();
        store.save(&kp).unwrap();

        let addr = store.address().unwrap();
        assert_eq!(addr, expected_addr);
    }

    #[cfg(unix)]
    #[test]
    fn wallet_file_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let store = make_store(&tmp);

        let kp = WalletKeypair::generate();
        store.save(&kp).unwrap();

        let perms = fs::metadata(store.wallet_path()).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "Wallet file must be owner-only (0600)"
        );
    }
}
