//! TUI session identity — UID generation, HMAC signing, and live
//! connection registry.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ── TUI entry ────────────────────────────────────────────────────

/// A connected TUI client.
#[derive(Debug, Clone)]
pub struct TuiEntry {
    pub tui_id: String,
    pub connected_at: DateTime<Utc>,
    pub peer_label: String,
    /// Transport protocol: `"unix"` or `"wss"`.
    pub transport: String,
    /// Full shell environment captured from the TUI process at connect time.
    /// Used to pass the user's real env (PATH, SSH_AUTH_SOCK, etc.) through
    /// to subprocesses spawned by the daemon on their behalf.
    pub env: HashMap<String, String>,
}

// ── Registry ─────────────────────────────────────────────────────

/// Which registration of a `tui_id` an entry is.
///
/// A reconnecting client re-registers its OWN id, so the id alone cannot
/// distinguish the live registration from one that has been superseded. A
/// connection whose teardown is still draining can therefore run AFTER its
/// successor has been adopted, and a key-only removal would evict the live
/// entry — taking the connected TUI's captured environment with it. This is the
/// fact that makes the two distinguishable: a teardown quotes the epoch it
/// registered under, so it can only ever remove its own registration.
pub type TuiEpoch = u64;

/// Daemon-wide registry of connected TUI clients.
/// **Source of truth** for live TUI connection state. Not persisted —
/// rebuilt on each daemon start from incoming `initialize` handshakes.
pub struct TuiRegistry {
    /// HMAC signing key loaded from `.secret_key`. `None` = signing
    /// disabled — UIDs are issued unsigned and reconnects trust claimed
    /// identities without verification.
    signing_key: Option<Vec<u8>>,
    /// Connected TUIs keyed by `tui_id`, each stamped with the epoch of the
    /// registration that installed it.
    connected: Mutex<HashMap<String, (TuiEpoch, TuiEntry)>>,
    /// Hands out the next registration epoch. Monotonic for the life of the
    /// registry, so an epoch identifies one registration and is never reused.
    next_epoch: AtomicU64,
}

impl TuiRegistry {
    /// Create a registry, attempting to load the signing key from
    /// `<config_dir>/.secret_key`. If the file is missing or
    /// unreadable, signing is silently disabled.
    pub fn new(config_dir: &Path) -> Self {
        let key_path = config_dir.join(".secret_key");
        let signing_key = std::fs::read_to_string(&key_path)
            .ok()
            .and_then(|hex_str| hex::decode(hex_str.trim()).ok())
            .filter(|key| !key.is_empty());

        Self {
            signing_key,
            connected: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub fn new_unsigned() -> Self {
        Self {
            signing_key: None,
            connected: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(0),
        }
    }

    /// Whether HMAC signing is enabled (`.secret_key` was loaded).
    pub fn signing_is_enabled(&self) -> bool {
        self.signing_key.is_some()
    }

    // ── UID generation ───────────────────────────────────────────

    /// Generate a short TUI ID: `tui_` + 8 hex chars (4 random bytes).
    pub fn generate_tui_id() -> String {
        let bytes: [u8; 4] = rand::random();
        format!("tui_{}", hex::encode(bytes))
    }

    /// Generate a TUI ID that is not currently in the registry.
    pub fn generate_unique_tui_id(&self) -> String {
        let connected = self.connected.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let id = Self::generate_tui_id();
            if !connected.contains_key(&id) {
                return id;
            }
        }
    }

    // ── HMAC signing ─────────────────────────────────────────────

    /// Sign a TUI ID with HMAC-SHA256. Returns `None` if signing is
    /// disabled.
    pub fn sign(&self, tui_id: &str) -> Option<String> {
        let key = self.signing_key.as_ref()?;
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(tui_id.as_bytes());
        Some(hex::encode(mac.finalize().into_bytes()))
    }

    /// Verify a TUI ID + signature. Returns `true` if:
    /// - Signing is disabled (trust all), OR
    /// - The signature is valid.
    pub fn verify(&self, tui_id: &str, sig: &str) -> bool {
        let Some(ref key) = self.signing_key else {
            return true;
        };
        let Ok(sig_bytes) = hex::decode(sig) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(tui_id.as_bytes());
        mac.verify_slice(&sig_bytes).is_ok()
    }

    // ── Registry operations ──────────────────────────────────────

    /// Register a connected TUI, displacing any earlier registration of the same
    /// id. Returns the epoch this registration owns; the caller must quote it
    /// back to [`Self::unregister`] so a late teardown cannot evict a successor.
    pub fn register(&self, entry: TuiEntry) -> TuiEpoch {
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
        self.connected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(entry.tui_id.clone(), (epoch, entry));
        epoch
    }

    /// Unregister a disconnected TUI, but ONLY while `epoch` is still the live
    /// registration for that id.
    ///
    /// A reconnect adopts the same id under a new epoch, and the displaced
    /// connection's teardown can run at any point after that — it may still be
    /// draining, or parked in a bounded write. Removing by key alone would let
    /// that late teardown evict the connection that replaced it.
    pub fn unregister(&self, tui_id: &str, epoch: TuiEpoch) {
        let mut connected = self.connected.lock().unwrap_or_else(|e| e.into_inner());
        if connected
            .get(tui_id)
            .is_some_and(|(live, _)| *live == epoch)
        {
            connected.remove(tui_id);
        }
    }

    /// Snapshot of all connected TUIs.
    pub fn list(&self) -> Vec<TuiEntry> {
        self.connected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// Return a clone of the environment captured from the TUI identified by
    /// `tui_id`, or `None` if the TUI is not currently connected.
    pub fn get_env(&self, tui_id: &str) -> Option<HashMap<String, String>> {
        self.connected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tui_id)
            .map(|(_, entry)| entry.env.clone())
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_tui_id_format() {
        let id = TuiRegistry::generate_tui_id();
        assert!(id.starts_with("tui_"), "expected tui_ prefix, got {id}");
        assert_eq!(id.len(), 12, "tui_ + 8 hex chars = 12, got {}", id.len());
        // Hex chars only after prefix
        assert!(
            id[4..].chars().all(|c| c.is_ascii_hexdigit()),
            "non-hex chars in {id}"
        );
    }

    #[test]
    fn sign_verify_roundtrip() {
        let registry = TuiRegistry {
            signing_key: Some(vec![0xAB; 32]),
            connected: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(0),
        };
        let id = "tui_deadbeef";
        let sig = registry.sign(id).expect("signing should succeed");
        assert!(registry.verify(id, &sig), "roundtrip verify failed");
    }

    #[test]
    fn verify_rejects_tampered_sig() {
        let registry = TuiRegistry {
            signing_key: Some(vec![0xAB; 32]),
            connected: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(0),
        };
        let id = "tui_deadbeef";
        let sig = registry.sign(id).unwrap();
        // Flip a character
        let mut tampered = sig.clone();
        let replacement = if tampered.ends_with('0') { 'f' } else { '0' };
        tampered.pop();
        tampered.push(replacement);
        assert!(!registry.verify(id, &tampered), "tampered sig should fail");
    }

    #[test]
    fn verify_rejects_wrong_id() {
        let registry = TuiRegistry {
            signing_key: Some(vec![0xAB; 32]),
            connected: Mutex::new(HashMap::new()),
            next_epoch: AtomicU64::new(0),
        };
        let sig = registry.sign("tui_aaaaaaaa").unwrap();
        assert!(
            !registry.verify("tui_bbbbbbbb", &sig),
            "wrong ID should fail"
        );
    }

    #[test]
    fn verify_without_key_trusts_all() {
        let registry = TuiRegistry::new_unsigned();
        assert!(registry.verify("tui_anything", "bogus_sig"));
    }

    #[test]
    fn signing_disabled_returns_none() {
        let registry = TuiRegistry::new_unsigned();
        assert!(registry.sign("tui_test").is_none());
        assert!(!registry.signing_is_enabled());
    }

    #[test]
    fn register_unregister_lifecycle() {
        let registry = TuiRegistry::new_unsigned();
        assert!(registry.list().is_empty());

        let epoch = registry.register(TuiEntry {
            tui_id: "tui_aabb0011".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env: HashMap::new(),
        });
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].tui_id, "tui_aabb0011");

        registry.unregister("tui_aabb0011", epoch);
        assert!(registry.list().is_empty());
    }

    fn entry(tui_id: &str, peer_label: &str) -> TuiEntry {
        TuiEntry {
            tui_id: tui_id.to_string(),
            connected_at: Utc::now(),
            peer_label: peer_label.to_string(),
            transport: "wss".to_string(),
            env: HashMap::new(),
        }
    }

    // A reconnect adopts the same TUI id, so the displaced connection's teardown
    // arrives with a stale epoch. It must not evict the live registration: doing
    // so drops the connected TUI's captured environment while it is still using
    // the daemon, and repeated flaps make it recur.
    #[test]
    fn a_superseded_teardown_cannot_evict_the_connection_that_replaced_it() {
        let registry = TuiRegistry::new_unsigned();
        let id = "tui_adopted1";

        let epoch_a = registry.register(entry(id, "session-a"));
        let epoch_b = registry.register(entry(id, "session-b"));
        assert_ne!(epoch_a, epoch_b, "each registration owns its own epoch");

        // Session A's cleanup finally runs, after B has been adopted.
        registry.unregister(id, epoch_a);

        let live = registry.list();
        assert_eq!(live.len(), 1, "the live registration must survive");
        assert_eq!(
            live[0].peer_label, "session-b",
            "the surviving entry must be the successor, not the displaced one"
        );
        assert!(
            registry.get_env(id).is_some(),
            "the live TUI must still resolve its captured environment"
        );

        // The normal path still works: B's own teardown removes B.
        registry.unregister(id, epoch_b);
        assert!(
            registry.list().is_empty(),
            "the live registration's own teardown must still remove it"
        );
    }

    // Flaps accumulate displaced connections; none of their teardowns, in any
    // order, may take the live entry with them.
    #[test]
    fn repeated_flaps_leave_the_current_registration_intact() {
        let registry = TuiRegistry::new_unsigned();
        let id = "tui_flapping";

        let stale: Vec<TuiEpoch> = (0..5)
            .map(|_| registry.register(entry(id, "stale")))
            .collect();
        let live = registry.register(entry(id, "live"));

        for epoch in stale {
            registry.unregister(id, epoch);
        }
        assert_eq!(
            registry.list().len(),
            1,
            "every stale teardown must be inert"
        );
        assert_eq!(registry.list()[0].peer_label, "live");

        registry.unregister(id, live);
        assert!(registry.list().is_empty());
    }

    // An epoch is only ever meaningful for its own id.
    #[test]
    fn an_epoch_from_one_id_does_not_remove_another() {
        let registry = TuiRegistry::new_unsigned();
        let first = registry.register(entry("tui_first000", "a"));
        registry.register(entry("tui_second00", "b"));

        registry.unregister("tui_second00", first);
        assert_eq!(registry.list().len(), 2, "a foreign epoch must not match");
    }

    #[test]
    fn unregister_unknown_is_noop() {
        let registry = TuiRegistry::new_unsigned();
        registry.unregister("tui_nonexistent", 0); // must not panic
    }

    #[test]
    fn generate_unique_avoids_existing() {
        let registry = TuiRegistry::new_unsigned();
        // Pre-populate with a known ID
        registry.register(TuiEntry {
            tui_id: "tui_00000000".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env: HashMap::new(),
        });
        // generate_unique should return something different
        let id = registry.generate_unique_tui_id();
        assert_ne!(id, "tui_00000000");
    }

    // ── TUI env passthrough tests ─────────────────────────────────

    #[test]
    fn tui_entry_stores_env() {
        let registry = TuiRegistry::new_unsigned();
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "my_value".to_string());
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-secret".to_string());

        registry.register(TuiEntry {
            tui_id: "tui_aabbccdd".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env,
        });

        let entries = registry.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].env.get("MY_VAR").map(|s| s.as_str()),
            Some("my_value")
        );
        assert_eq!(
            entries[0].env.get("ANTHROPIC_API_KEY").map(|s| s.as_str()),
            Some("sk-secret"),
            "full env should be stored without filtering"
        );
    }

    #[test]
    fn tui_entry_env_defaults_to_empty() {
        // Entries with no env (e.g. old clients) should work fine
        let registry = TuiRegistry::new_unsigned();
        registry.register(TuiEntry {
            tui_id: "tui_11223344".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env: HashMap::new(),
        });

        let entries = registry.list();
        assert!(entries[0].env.is_empty());
    }

    #[test]
    fn tui_entry_env_dropped_on_unregister() {
        let registry = TuiRegistry::new_unsigned();
        let mut env = HashMap::new();
        env.insert("SOME_VAR".to_string(), "some_value".to_string());

        let epoch = registry.register(TuiEntry {
            tui_id: "tui_deadbeef".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env,
        });
        assert_eq!(registry.list().len(), 1);

        registry.unregister("tui_deadbeef", epoch);
        assert!(
            registry.list().is_empty(),
            "env should be dropped with entry"
        );
    }

    #[test]
    fn tui_entry_env_survives_clone() {
        // TuiEntry derives Clone — env must be included
        let mut env = HashMap::new();
        env.insert("CLONED_VAR".to_string(), "cloned_value".to_string());

        let entry = TuiEntry {
            tui_id: "tui_cafebabe".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env,
        };
        let cloned = entry.clone();
        assert_eq!(
            cloned.env.get("CLONED_VAR").map(|s| s.as_str()),
            Some("cloned_value")
        );
    }

    #[test]
    fn get_env_returns_env_for_connected_tui() {
        let registry = TuiRegistry::new_unsigned();
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin:/usr/local/bin".to_string());
        env.insert("SSH_AUTH_SOCK".to_string(), "/tmp/ssh.sock".to_string());

        registry.register(TuiEntry {
            tui_id: "tui_getenv01".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env,
        });

        let got = registry.get_env("tui_getenv01").expect("should find env");
        assert_eq!(
            got.get("PATH").map(|s| s.as_str()),
            Some("/usr/bin:/usr/local/bin")
        );
        assert_eq!(
            got.get("SSH_AUTH_SOCK").map(|s| s.as_str()),
            Some("/tmp/ssh.sock")
        );
    }

    #[test]
    fn get_env_returns_none_for_unknown_tui() {
        let registry = TuiRegistry::new_unsigned();
        assert!(registry.get_env("tui_nothere").is_none());
    }

    #[test]
    fn get_env_returns_none_after_unregister() {
        let registry = TuiRegistry::new_unsigned();
        let mut env = HashMap::new();
        env.insert("SOME_VAR".to_string(), "val".to_string());
        let epoch = registry.register(TuiEntry {
            tui_id: "tui_gone0001".to_string(),
            connected_at: Utc::now(),
            peer_label: "test".to_string(),
            transport: "unix".to_string(),
            env,
        });
        assert!(registry.get_env("tui_gone0001").is_some());
        registry.unregister("tui_gone0001", epoch);
        assert!(registry.get_env("tui_gone0001").is_none());
    }
}
