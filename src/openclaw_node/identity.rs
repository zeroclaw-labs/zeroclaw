/// OpenClaw device identity — Ed25519 key generation and v3 auth signing
use anyhow::{anyhow, Result};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Device identity with persisted Ed25519 keypair
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub device_id: String,
    private_key_bytes: Vec<u8>, // raw 32-byte Ed25519 seed
    public_key_bytes: Vec<u8>,  // raw 32-byte public key
}

/// Persistent identity file format (JSON)
#[derive(Debug, Serialize, Deserialize)]
struct IdentityFile {
    device_id: String,
    private_key: String, // base64-encoded
    public_key: String,  // base64-encoded
}

impl DeviceIdentity {
    /// Load identity from file, or generate and save new one if path doesn't exist
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let identity = Self::generate()?;
            identity.save(path)?;
            Ok(identity)
        }
    }

    /// Load identity from file
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read_to_string(path)
            .map_err(|e| anyhow!("failed to read device identity file: {}", e))?;
        let file: IdentityFile = serde_json::from_str(&data)
            .map_err(|e| anyhow!("failed to parse device identity file: {}", e))?;

        let private_key_bytes =
            base64_url::decode(&file.private_key)
                .map_err(|e| anyhow!("failed to decode private key: {}", e))?;
        let public_key_bytes = base64_url::decode(&file.public_key)
            .map_err(|e| anyhow!("failed to decode public key: {}", e))?;

        Ok(DeviceIdentity {
            device_id: file.device_id,
            private_key_bytes,
            public_key_bytes,
        })
    }

    /// Save identity to file
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| anyhow!("failed to create identity directory: {}", e))?;
        }
        let file = IdentityFile {
            device_id: self.device_id.clone(),
            private_key: base64_url::encode(&self.private_key_bytes),
            public_key: base64_url::encode(&self.public_key_bytes),
        };
        let data = serde_json::to_string_pretty(&file)
            .map_err(|e| anyhow!("failed to serialize identity: {}", e))?;
        fs::write(path, data)
            .map_err(|e| anyhow!("failed to write identity file: {}", e))?;
        Ok(())
    }

    /// Generate new device identity with random Ed25519 keypair
    pub fn generate() -> Result<Self> {
        let rng = ring::rand::SystemRandom::new();
        let private_key_bytes = {
            let mut bytes = [0u8; 32];
            ring::rand::SecureRandom::fill(&rng, &mut bytes)
                .map_err(|_| anyhow!("failed to generate random bytes"))?;
            bytes.to_vec()
        };

        let key_pair = Ed25519KeyPair::from_seed_unchecked(&private_key_bytes)
            .map_err(|_| anyhow!("failed to create Ed25519 key pair"))?;
        let public_key_bytes = key_pair.public_key().as_ref().to_vec();

        Ok(DeviceIdentity {
            device_id: uuid::Uuid::new_v4().to_string(),
            private_key_bytes,
            public_key_bytes,
        })
    }

    /// Get public key in base64url encoding
    pub fn public_key_base64url(&self) -> String {
        base64_url::encode(&self.public_key_bytes)
    }

    /// Get device ID
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Sign a v3 auth payload and return signature in base64url
    pub fn sign_v3_payload(&self, payload: &str) -> Result<String> {
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&self.private_key_bytes)
            .map_err(|_| anyhow!("failed to create Ed25519 key pair for signing"))?;
        let signature = key_pair.sign(payload.as_bytes());
        Ok(base64_url::encode(signature.as_ref()))
    }

    /// Build and sign v3 device auth payload
    ///
    /// v3 format: v3|{deviceId}|{clientId}|{clientMode}|{role}|{scopes.join(",")}|{signedAtMs}|{token}|{nonce}|{platform}|{deviceFamily}
    pub fn build_v3_signature(
        &self,
        client_id: &str,
        client_mode: &str,
        role: &str,
        scopes: &[&str],
        signed_at_ms: u64,
        token: &str,
        nonce: &str,
        platform: &str,
        device_family: Option<&str>,
    ) -> Result<String> {
        let scopes_str = scopes.join(",");
        let device_family_str = device_family.unwrap_or("");
        let payload = format!(
            "v3|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.device_id,
            client_id,
            client_mode,
            role,
            scopes_str,
            signed_at_ms,
            token,
            nonce,
            platform,
            device_family_str
        );
        self.sign_v3_payload(&payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_identity() {
        let id = DeviceIdentity::generate().unwrap();
        assert!(!id.device_id.is_empty());
        assert_eq!(id.private_key_bytes.len(), 32);
        assert_eq!(id.public_key_bytes.len(), 32);
    }

    #[test]
    fn test_persist_and_load() {
        let tmpdir = TempDir::new().unwrap();
        let path = tmpdir.path().join("identity.json");

        let id1 = DeviceIdentity::generate().unwrap();
        id1.save(&path).unwrap();

        let id2 = DeviceIdentity::load(&path).unwrap();
        assert_eq!(id1.device_id, id2.device_id);
        assert_eq!(id1.private_key_bytes, id2.private_key_bytes);
        assert_eq!(id1.public_key_bytes, id2.public_key_bytes);
    }

    #[test]
    fn test_v3_signature() {
        let id = DeviceIdentity::generate().unwrap();
        let sig = id
            .build_v3_signature("node-host", "node", "node", &[], 1000, "token123", "nonce456", "linux", None)
            .unwrap();
        assert!(!sig.is_empty());
    }
}
