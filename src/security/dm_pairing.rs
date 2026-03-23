//! Per-channel DM pairing for messenger authorization.
//!
//! Ported from RustyClaw's `PairingManager`. This module provides per-sender,
//! per-channel authorization for DM-based channels (Telegram, Discord, etc.).
//!
//! When a channel has no pre-configured allowlist, unknown senders receive a
//! pairing code. The operator approves the code, and the sender is added to a
//! persistent allowlist stored as JSON.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default pairing code expiry: 5 minutes.
const DEFAULT_CODE_EXPIRY_SECS: u64 = 300;

/// Length of generated pairing codes.
const PAIRING_CODE_LENGTH: usize = 8;

/// An authorized sender entry in the allowlist.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AllowlistEntry {
    /// Display name for the sender.
    pub name: String,
    /// Unix timestamp when the sender was paired.
    pub paired_at: u64,
    /// Optional notes (e.g. who approved, reason).
    pub notes: Option<String>,
}

/// A pending pairing code waiting for verification.
#[derive(Debug, Clone)]
struct PendingCode {
    code: String,
    expires_at: u64,
}

/// Manages per-channel, per-sender DM pairing.
///
/// Each sender on each channel gets a unique pairing code. Once verified and
/// approved, the sender is added to a persistent JSON allowlist.
#[derive(Debug)]
pub struct DmPairingManager {
    /// Path to the allowlist JSON file.
    allowlist_path: PathBuf,
    /// Authorized senders: key = `"channel_type:sender_id"`.
    allowlist: HashMap<String, AllowlistEntry>,
    /// Pending codes: key = `"channel_type:sender_id"`.
    pending: HashMap<String, PendingCode>,
    /// Code expiry duration in seconds.
    code_expiry_secs: u64,
}

impl DmPairingManager {
    /// Create a new manager, loading the allowlist from disk if it exists.
    pub fn new(allowlist_path: impl Into<PathBuf>) -> Self {
        let allowlist_path = allowlist_path.into();
        let allowlist = load_allowlist(&allowlist_path).unwrap_or_default();
        Self {
            allowlist_path,
            allowlist,
            pending: HashMap::new(),
            code_expiry_secs: DEFAULT_CODE_EXPIRY_SECS,
        }
    }

    /// Check whether a sender is authorized on a given channel.
    pub fn is_authorized(&self, channel_type: &str, sender_id: &str) -> bool {
        let key = make_key(channel_type, sender_id);
        self.allowlist.contains_key(&key)
    }

    /// Generate (or return existing) pairing code for a sender.
    ///
    /// Returns `None` if the sender is already authorized.
    pub fn generate_code(&mut self, channel_type: &str, sender_id: &str) -> Option<String> {
        if self.is_authorized(channel_type, sender_id) {
            return None;
        }

        let key = make_key(channel_type, sender_id);
        let now = now_secs();

        // Reuse existing non-expired code
        if let Some(pending) = self.pending.get(&key) {
            if pending.expires_at > now {
                return Some(pending.code.clone());
            }
        }

        let code = generate_random_code(PAIRING_CODE_LENGTH);
        self.pending.insert(
            key,
            PendingCode {
                code: code.clone(),
                expires_at: now + self.code_expiry_secs,
            },
        );

        Some(code)
    }

    /// Verify a submitted pairing code.
    pub fn verify_code(&mut self, channel_type: &str, sender_id: &str, submitted: &str) -> bool {
        let key = make_key(channel_type, sender_id);
        let now = now_secs();

        self.cleanup_expired_codes();

        if let Some(pending) = self.pending.get(&key) {
            if pending.expires_at > now && pending.code == submitted {
                return true;
            }
        }

        false
    }

    /// Approve a sender and add them to the persistent allowlist.
    pub fn approve_sender(
        &mut self,
        channel_type: &str,
        sender_id: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        let key = make_key(channel_type, sender_id);
        self.pending.remove(&key);

        self.allowlist.insert(
            key,
            AllowlistEntry {
                name: name.to_string(),
                paired_at: now_secs(),
                notes: None,
            },
        );

        self.save_allowlist()
    }

    /// Revoke a sender's access.
    pub fn revoke_sender(&mut self, channel_type: &str, sender_id: &str) -> anyhow::Result<()> {
        let key = make_key(channel_type, sender_id);
        self.allowlist.remove(&key);
        self.save_allowlist()
    }

    /// List all authorized senders.
    pub fn list_authorized(&self) -> &HashMap<String, AllowlistEntry> {
        &self.allowlist
    }

    /// List pending codes with their expiry times.
    pub fn list_pending(&self) -> Vec<(String, u64)> {
        let now = now_secs();
        self.pending
            .iter()
            .filter(|(_, p)| p.expires_at > now)
            .map(|(k, p)| (k.clone(), p.expires_at))
            .collect()
    }

    /// Remove expired codes from the pending map.
    fn cleanup_expired_codes(&mut self) {
        let now = now_secs();
        self.pending.retain(|_, p| p.expires_at > now);
    }

    /// Persist the allowlist to disk as JSON.
    fn save_allowlist(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.allowlist_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.allowlist)?;
        std::fs::write(&self.allowlist_path, json)?;
        Ok(())
    }
}

/// Load allowlist from a JSON file.
fn load_allowlist(path: &Path) -> anyhow::Result<HashMap<String, AllowlistEntry>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = std::fs::read_to_string(path)?;
    if data.trim().is_empty() {
        return Ok(HashMap::new());
    }
    Ok(serde_json::from_str(&data)?)
}

fn make_key(channel_type: &str, sender_id: &str) -> String {
    format!("{}:{}", channel_type, sender_id)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Generate a random alphanumeric code, excluding ambiguous characters.
fn generate_random_code(length: usize) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    const CHARSET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut code = String::with_capacity(length);
    for i in 0..length {
        // Use RandomState for OS-seeded randomness without adding rand dependency
        let state = RandomState::new();
        let mut hasher = state.build_hasher();
        hasher.write_usize(i);
        let idx = (hasher.finish() as usize) % CHARSET.len();
        code.push(CHARSET[idx] as char);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_allowlist() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dm_allowlist.json");
        (dir, path)
    }

    #[test]
    fn new_sender_is_not_authorized() {
        let (_dir, path) = temp_allowlist();
        let mgr = DmPairingManager::new(&path);
        assert!(!mgr.is_authorized("telegram", "12345"));
    }

    #[test]
    fn full_pairing_flow() {
        let (_dir, path) = temp_allowlist();
        let mut mgr = DmPairingManager::new(&path);

        // Not authorized initially
        assert!(!mgr.is_authorized("telegram", "user1"));

        // Generate code
        let code = mgr.generate_code("telegram", "user1").unwrap();
        assert_eq!(code.len(), PAIRING_CODE_LENGTH);

        // Same code returned on second call
        let code2 = mgr.generate_code("telegram", "user1").unwrap();
        assert_eq!(code, code2);

        // Wrong code fails
        assert!(!mgr.verify_code("telegram", "user1", "WRONGCODE"));

        // Correct code succeeds
        assert!(mgr.verify_code("telegram", "user1", &code));

        // Approve
        mgr.approve_sender("telegram", "user1", "Test User")
            .unwrap();
        assert!(mgr.is_authorized("telegram", "user1"));

        // No code generated for authorized user
        assert!(mgr.generate_code("telegram", "user1").is_none());
    }

    #[test]
    fn revoke_sender() {
        let (_dir, path) = temp_allowlist();
        let mut mgr = DmPairingManager::new(&path);

        mgr.approve_sender("discord", "abc", "Alice").unwrap();
        assert!(mgr.is_authorized("discord", "abc"));

        mgr.revoke_sender("discord", "abc").unwrap();
        assert!(!mgr.is_authorized("discord", "abc"));
    }

    #[test]
    fn allowlist_persists() {
        let (_dir, path) = temp_allowlist();

        {
            let mut mgr = DmPairingManager::new(&path);
            mgr.approve_sender("slack", "U123", "Bob").unwrap();
        }

        // New manager loads from disk
        let mgr = DmPairingManager::new(&path);
        assert!(mgr.is_authorized("slack", "U123"));
    }

    #[test]
    fn different_channels_are_isolated() {
        let (_dir, path) = temp_allowlist();
        let mut mgr = DmPairingManager::new(&path);

        mgr.approve_sender("telegram", "user1", "Alice").unwrap();

        assert!(mgr.is_authorized("telegram", "user1"));
        assert!(!mgr.is_authorized("discord", "user1"));
        assert!(!mgr.is_authorized("telegram", "user2"));
    }

    #[test]
    fn code_charset_is_unambiguous() {
        let code = generate_random_code(100);
        for c in code.chars() {
            assert!(
                !"IOL01".contains(c),
                "Code contains ambiguous character: {c}"
            );
        }
    }

    #[test]
    fn list_authorized_and_pending() {
        let (_dir, path) = temp_allowlist();
        let mut mgr = DmPairingManager::new(&path);

        mgr.generate_code("telegram", "u1");
        mgr.approve_sender("discord", "u2", "Carol").unwrap();

        assert_eq!(mgr.list_authorized().len(), 1);
        assert_eq!(mgr.list_pending().len(), 1);
    }

    #[test]
    fn empty_allowlist_file() {
        let (_dir, path) = temp_allowlist();
        std::fs::write(&path, "").unwrap();
        let mgr = DmPairingManager::new(&path);
        assert!(mgr.list_authorized().is_empty());
    }
}
