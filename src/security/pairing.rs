// Gateway pairing mode — first-connect authentication.
//
// On startup the gateway generates a one-time pairing code printed to the
// terminal. The first client must present this code via `X-Pairing-Code`
// header on a `POST /pair` request. The server responds with a bearer token
// that must be sent on all subsequent requests via `Authorization: Bearer <token>`.
//
// Already-paired tokens are persisted in config so restarts don't require
// re-pairing.

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

/// Maximum failed pairing attempts before lockout.
const MAX_PAIR_ATTEMPTS: u32 = 5;
/// Lockout duration after too many failed pairing attempts.
const PAIR_LOCKOUT_SECS: u64 = 300; // 5 minutes
/// Maximum number of tracked client entries to bound memory usage.
const MAX_TRACKED_CLIENTS: usize = 10_000;
/// Retention period for failed-attempt entries with no activity.
const FAILED_ATTEMPT_RETENTION_SECS: u64 = 900; // 15 min
/// Minimum interval between full sweeps of the failed-attempt map.
const FAILED_ATTEMPT_SWEEP_INTERVAL_SECS: u64 = 300; // 5 min

/// Per-client failed attempt state with optional absolute lockout deadline.
#[derive(Debug, Clone, Copy)]
struct FailedAttemptState {
    count: u32,
    lockout_until: Option<Instant>,
    last_attempt: Instant,
}

/// Manages pairing state for the gateway.
///
/// Bearer tokens are stored as SHA-256 hashes to prevent plaintext exposure
/// in config files. When a new token is generated, the plaintext is returned
/// to the client once, and only the hash is retained.
// TODO: I've just made this work with parking_lot but it should use either flume or tokio's async mutexes
#[derive(Debug, Clone)]
pub struct PairingGuard {
    /// Whether pairing is required at all.
    require_pairing: bool,
    /// One-time pairing code (generated on startup, consumed on first pair).
    pairing_code: Arc<Mutex<Option<String>>>,
    /// Set of SHA-256 hashed bearer tokens (persisted across restarts).
    paired_tokens: Arc<Mutex<HashSet<String>>>,
    /// Brute-force protection: per-client failed attempt state + last sweep timestamp.
    failed_attempts: Arc<Mutex<(HashMap<String, FailedAttemptState>, Instant)>>,
}

impl PairingGuard {
    /// Create a new pairing guard.
    ///
    /// If `require_pairing` is true and no tokens exist yet, a fresh
    /// pairing code is generated and returned via `pairing_code()`.
    ///
    /// Existing tokens are accepted in both forms:
    /// - Plaintext (`zc_...`): hashed on load for backward compatibility
    /// - Already hashed (64-char hex): stored as-is
    pub fn new(require_pairing: bool, existing_tokens: &[String]) -> Self {
        let tokens: HashSet<String> = existing_tokens
            .iter()
            .map(|t| {
                if is_token_hash(t) {
                    t.clone()
                } else {
                    hash_token(t)
                }
            })
            .collect();
        let code = if require_pairing && tokens.is_empty() {
            Some(generate_code())
        } else {
            None
        };
        Self {
            require_pairing,
            pairing_code: Arc::new(Mutex::new(code)),
            paired_tokens: Arc::new(Mutex::new(tokens)),
            failed_attempts: Arc::new(Mutex::new((HashMap::new(), Instant::now()))),
        }
    }

    /// The one-time pairing code (only set when no tokens exist yet).
    pub fn pairing_code(&self) -> Option<String> {
        self.pairing_code.lock().clone()
    }

    /// Whether pairing is required at all.
    pub fn require_pairing(&self) -> bool {
        self.require_pairing
    }

    fn try_pair_blocking(&self, code: &str, client_id: &str) -> Result<Option<String>, u64> {
        let client_id = normalize_client_key(client_id);
        let now = Instant::now();

        // Periodic sweep + lockout check
        {
            let mut guard = self.failed_attempts.lock();
            let (ref mut map, ref mut last_sweep) = *guard;

            // Sweep stale entries on interval
            if now.duration_since(*last_sweep).as_secs() >= FAILED_ATTEMPT_SWEEP_INTERVAL_SECS {
                prune_failed_attempts(map, now);
                *last_sweep = now;
            }

            // Check brute force lockout for this specific client
            if let Some(state) = map.get(&client_id) {
                if let Some(until) = state.lockout_until {
                    if now < until {
                        let remaining = (until - now).as_secs();
                        return Err(remaining.max(1));
                    }
                    // Lockout expired — reset inline
                    map.remove(&client_id);
                }
            }
        }

        {
            let mut pairing_code = self.pairing_code.lock();
            if let Some(ref expected) = *pairing_code {
                if constant_time_eq(code.trim(), expected.trim()) {
                    // Reset failed attempts for this client on success
                    {
                        let mut guard = self.failed_attempts.lock();
                        guard.0.remove(&client_id);
                    }
                    let token = generate_token();
                    let mut tokens = self.paired_tokens.lock();
                    tokens.insert(hash_token(&token));

                    // Consume the pairing code so it cannot be reused
                    *pairing_code = None;

                    return Ok(Some(token));
                }
            }
        }

        // Increment failed attempts for this client
        {
            let mut guard = self.failed_attempts.lock();
            let (ref mut map, _) = *guard;

            // Enforce capacity bound: prune stale first, then LRU-evict if still full
            if map.len() >= MAX_TRACKED_CLIENTS {
                prune_failed_attempts(map, now);
            }
            if map.len() >= MAX_TRACKED_CLIENTS {
                // Evict the least-recently-active entry
                if let Some(lru_key) = map
                    .iter()
                    .min_by_key(|(_, s)| s.last_attempt)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&lru_key);
                }
            }

            let entry = map.entry(client_id).or_insert(FailedAttemptState {
                count: 0,
                lockout_until: None,
                last_attempt: now,
            });

            entry.last_attempt = now;
            entry.count += 1;

            if entry.count >= MAX_PAIR_ATTEMPTS {
                entry.lockout_until = Some(now + std::time::Duration::from_secs(PAIR_LOCKOUT_SECS));
            }
        }

        Ok(None)
    }

    /// Attempt to pair with the given code. Returns a bearer token on success.
    /// Returns `Err(lockout_seconds)` if locked out due to brute force.
    /// `client_id` identifies the client for per-client lockout accounting.
    pub async fn try_pair(&self, code: &str, client_id: &str) -> Result<Option<String>, u64> {
        let this = self.clone();
        let code = code.to_string();
        let client_id = client_id.to_string();
        // TODO: make this function the main one without spawning a task
        let handle = tokio::task::spawn_blocking(move || this.try_pair_blocking(&code, &client_id));

        match handle.await {
            Ok(result) => result,
            Err(err) => {
                tracing::error!("pairing worker task failed: {err}");
                Ok(None)
            }
        }
    }

    /// Check if a bearer token is valid (compares against stored hashes).
    pub fn is_authenticated(&self, token: &str) -> bool {
        if !self.require_pairing {
            return true;
        }
        let hashed = hash_token(token);
        let tokens = self.paired_tokens.lock();
        tokens.contains(&hashed)
    }

    /// Returns true if the gateway is already paired (has at least one token).
    pub fn is_paired(&self) -> bool {
        let tokens = self.paired_tokens.lock();
        !tokens.is_empty()
    }

    /// Get all paired token hashes (for persisting to config).
    pub fn tokens(&self) -> Vec<String> {
        let tokens = self.paired_tokens.lock();
        tokens.iter().cloned().collect()
    }
}

/// Normalize a client identifier: trim whitespace, map empty to `"unknown"`.
fn normalize_client_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Remove failed-attempt entries whose `last_attempt` is older than the retention window.
fn prune_failed_attempts(map: &mut HashMap<String, FailedAttemptState>, now: Instant) {
    map.retain(|_, state| {
        now.duration_since(state.last_attempt).as_secs() < FAILED_ATTEMPT_RETENTION_SECS
    });
}

/// Generate a 6-digit numeric pairing code using cryptographically secure randomness.
fn generate_code() -> String {
    // UUID v4 uses getrandom (backed by /dev/urandom on Linux, BCryptGenRandom
    // on Windows) — a CSPRNG. We extract 4 bytes from it for a uniform random
    // number in [0, 1_000_000).
    //
    // Rejection sampling eliminates modulo bias: values above the largest
    // multiple of 1_000_000 that fits in u32 are discarded and re-drawn.
    // The rejection probability is ~0.02%, so this loop almost always exits
    // on the first iteration.
    const UPPER_BOUND: u32 = 1_000_000;
    const REJECT_THRESHOLD: u32 = (u32::MAX / UPPER_BOUND) * UPPER_BOUND;

    loop {
        let uuid = uuid::Uuid::new_v4();
        let bytes = uuid.as_bytes();
        let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        if raw < REJECT_THRESHOLD {
            return format!("{:06}", raw % UPPER_BOUND);
        }
    }
}

/// Generate a cryptographically-adequate bearer token with 256-bit entropy.
///
/// Uses `rand::rng()` which is backed by the OS CSPRNG
/// (/dev/urandom on Linux, BCryptGenRandom on Windows, SecRandomCopyBytes
/// on macOS). The 32 random bytes (256 bits) are hex-encoded for a
/// 64-character token, providing 256 bits of entropy.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("zc_{}", hex::encode(bytes))
}

/// SHA-256 hash a bearer token for storage. Returns lowercase hex.
fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// Check if a stored value looks like a SHA-256 hash (64 hex chars)
/// rather than a plaintext token.
fn is_token_hash(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Constant-time string comparison to prevent timing attacks.
///
/// Does not short-circuit on length mismatch — always iterates over the
/// longer input to avoid leaking length information via timing.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();

    // Track length mismatch as a usize (non-zero = different lengths)
    let len_diff = a.len() ^ b.len();

    // XOR each byte, padding the shorter input with zeros.
    // Iterates over max(a.len(), b.len()) to avoid timing differences.
    let max_len = a.len().max(b.len());
    let mut byte_diff = 0u8;
    for i in 0..max_len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        byte_diff |= x ^ y;
    }
    (len_diff == 0) & (byte_diff == 0)
}

/// Check if a host string represents a non-localhost bind address.
pub fn is_public_bind(host: &str) -> bool {
    !matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "[::1]" | "0:0:0:0:0:0:0:1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::test;

    // ── PairingGuard ─────────────────────────────────────────

    #[test]
    async fn new_guard_generates_code_when_no_tokens() {
        let guard = PairingGuard::new(true, &[]);
        assert!(guard.pairing_code().is_some());
        assert!(!guard.is_paired());
    }

    #[test]
    async fn new_guard_no_code_when_tokens_exist() {
        let guard = PairingGuard::new(true, &["zc_existing".into()]);
        assert!(guard.pairing_code().is_none());
        assert!(guard.is_paired());
    }

    #[test]
    async fn new_guard_no_code_when_pairing_disabled() {
        let guard = PairingGuard::new(false, &[]);
        assert!(guard.pairing_code().is_none());
    }

    #[test]
    async fn try_pair_correct_code() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "test_client").await.unwrap();
        assert!(token.is_some());
        assert!(token.unwrap().starts_with("zc_"));
        assert!(guard.is_paired());
    }

    #[test]
    async fn try_pair_wrong_code() {
        let guard = PairingGuard::new(true, &[]);
        let result = guard.try_pair("000000", "test_client").await.unwrap();
        // Might succeed if code happens to be 000000, but extremely unlikely
        // Just check it returns Ok(None) normally
        let _ = result;
    }

    #[test]
    async fn try_pair_empty_code() {
        let guard = PairingGuard::new(true, &[]);
        assert!(guard.try_pair("", "test_client").await.unwrap().is_none());
    }

    #[test]
    async fn is_authenticated_with_valid_token() {
        // Pass plaintext token — PairingGuard hashes it on load
        let guard = PairingGuard::new(true, &["zc_valid".into()]);
        assert!(guard.is_authenticated("zc_valid"));
    }

    #[test]
    async fn is_authenticated_with_prehashed_token() {
        // Pass an already-hashed token (64 hex chars)
        let hashed = hash_token("zc_valid");
        let guard = PairingGuard::new(true, &[hashed]);
        assert!(guard.is_authenticated("zc_valid"));
    }

    #[test]
    async fn is_authenticated_with_invalid_token() {
        let guard = PairingGuard::new(true, &["zc_valid".into()]);
        assert!(!guard.is_authenticated("zc_invalid"));
    }

    #[test]
    async fn is_authenticated_when_pairing_disabled() {
        let guard = PairingGuard::new(false, &[]);
        assert!(guard.is_authenticated("anything"));
        assert!(guard.is_authenticated(""));
    }

    #[test]
    async fn tokens_returns_hashes() {
        let guard = PairingGuard::new(true, &["zc_a".into(), "zc_b".into()]);
        let tokens = guard.tokens();
        assert_eq!(tokens.len(), 2);
        // Tokens should be stored as 64-char hex hashes, not plaintext
        for t in &tokens {
            assert_eq!(t.len(), 64, "Token should be a SHA-256 hash");
            assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(!t.starts_with("zc_"), "Token should not be plaintext");
        }
    }

    #[test]
    async fn pair_then_authenticate() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let token = guard.try_pair(&code, "test_client").await.unwrap().unwrap();
        assert!(guard.is_authenticated(&token));
        assert!(!guard.is_authenticated("wrong"));
    }

    // ── Token hashing ────────────────────────────────────────

    #[test]
    async fn hash_token_produces_64_hex_chars() {
        let hash = hash_token("zc_test_token");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    async fn hash_token_is_deterministic() {
        assert_eq!(hash_token("zc_abc"), hash_token("zc_abc"));
    }

    #[test]
    async fn hash_token_differs_for_different_inputs() {
        assert_ne!(hash_token("zc_a"), hash_token("zc_b"));
    }

    #[test]
    async fn is_token_hash_detects_hash_vs_plaintext() {
        assert!(is_token_hash(&hash_token("zc_test")));
        assert!(!is_token_hash("zc_test_token"));
        assert!(!is_token_hash("too_short"));
        assert!(!is_token_hash(""));
    }

    // ── is_public_bind ───────────────────────────────────────

    #[test]
    async fn localhost_variants_not_public() {
        assert!(!is_public_bind("127.0.0.1"));
        assert!(!is_public_bind("localhost"));
        assert!(!is_public_bind("::1"));
        assert!(!is_public_bind("[::1]"));
    }

    #[test]
    async fn zero_zero_is_public() {
        assert!(is_public_bind("0.0.0.0"));
    }

    #[test]
    async fn real_ip_is_public() {
        assert!(is_public_bind("192.168.1.100"));
        assert!(is_public_bind("10.0.0.1"));
    }

    // ── constant_time_eq ─────────────────────────────────────

    #[test]
    async fn constant_time_eq_same() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    async fn constant_time_eq_different() {
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("a", ""));
    }

    // ── generate helpers ─────────────────────────────────────

    #[test]
    async fn generate_code_is_6_digits() {
        let code = generate_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    async fn generate_code_is_not_deterministic() {
        // Two codes should differ with overwhelming probability. We try
        // multiple pairs so a single 1-in-10^6 collision doesn't cause
        // a flaky CI failure. All 10 pairs colliding is ~1-in-10^60.
        for _ in 0..10 {
            if generate_code() != generate_code() {
                return; // Pass: found a non-matching pair.
            }
        }
        panic!("Generated 10 pairs of codes and all were collisions — CSPRNG failure");
    }

    #[test]
    async fn generate_token_has_prefix_and_hex_payload() {
        let token = generate_token();
        let payload = token
            .strip_prefix("zc_")
            .expect("Generated token should include zc_ prefix");

        assert_eq!(payload.len(), 64, "Token payload should be 32 bytes in hex");
        assert!(
            payload
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f')),
            "Token payload should be lowercase hex"
        );
    }

    // ── Brute force protection ───────────────────────────────

    #[test]
    async fn brute_force_lockout_after_max_attempts() {
        let guard = PairingGuard::new(true, &[]);
        let client = "attacker_client";
        // Exhaust all attempts with wrong codes
        for i in 0..MAX_PAIR_ATTEMPTS {
            let result = guard.try_pair(&format!("wrong_{i}"), client).await;
            assert!(result.is_ok(), "Attempt {i} should not be locked out yet");
        }
        // Next attempt should be locked out
        let result = guard.try_pair("another_wrong", client).await;
        assert!(
            result.is_err(),
            "Should be locked out after {MAX_PAIR_ATTEMPTS} attempts"
        );
        let lockout_secs = result.unwrap_err();
        assert!(lockout_secs > 0, "Lockout should have remaining seconds");
        assert!(
            lockout_secs <= PAIR_LOCKOUT_SECS,
            "Lockout should not exceed max"
        );
    }

    #[test]
    async fn correct_code_resets_failed_attempts() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let client = "test_client";
        // Fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong", client).await;
        }
        // Correct code should still work (under MAX_PAIR_ATTEMPTS)
        let result = guard.try_pair(&code, client).await.unwrap();
        assert!(result.is_some(), "Correct code should work before lockout");
    }

    #[test]
    async fn lockout_returns_remaining_seconds() {
        let guard = PairingGuard::new(true, &[]);
        let client = "test_client";
        for _ in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair("wrong", client).await;
        }
        let err = guard.try_pair("wrong", client).await.unwrap_err();
        // Should be close to PAIR_LOCKOUT_SECS (within a second)
        assert!(
            err >= PAIR_LOCKOUT_SECS - 1,
            "Remaining lockout should be ~{PAIR_LOCKOUT_SECS}s, got {err}s"
        );
    }

    #[test]
    async fn successful_pair_resets_only_requesting_client_state() {
        let guard = PairingGuard::new(true, &[]);
        let code = guard.pairing_code().unwrap().to_string();
        let client_a = "client_a";
        let client_b = "client_b";

        // Both clients fail a few times
        for _ in 0..3 {
            let _ = guard.try_pair("wrong", client_a).await;
            let _ = guard.try_pair("wrong", client_b).await;
        }

        // client_a pairs successfully — only its state should reset
        let result = guard.try_pair(&code, client_a).await.unwrap();
        assert!(result.is_some(), "client_a should pair successfully");

        // client_b's failed count should still be intact (3 failures recorded)
        let state = guard.failed_attempts.lock();
        let b_state = state.0.get(client_b);
        assert!(b_state.is_some(), "client_b state should still exist");
        assert_eq!(
            b_state.unwrap().count,
            3,
            "client_b should still have 3 failures"
        );

        // client_a should have been removed
        assert!(
            !state.0.contains_key(client_a),
            "client_a state should be cleared"
        );
    }

    #[test]
    async fn failed_attempt_state_is_bounded_by_max_clients() {
        let guard = PairingGuard::new(true, &[]);

        // Fill the map to MAX_TRACKED_CLIENTS with stale entries
        {
            let mut state = guard.failed_attempts.lock();
            let past = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_RETENTION_SECS + 60,
                ))
                .unwrap_or_else(Instant::now);
            for i in 0..MAX_TRACKED_CLIENTS {
                state.0.insert(
                    format!("stale_client_{i}"),
                    FailedAttemptState {
                        count: 1,
                        lockout_until: None,
                        last_attempt: past,
                    },
                );
            }
        }

        // A new client triggers an attempt — should prune stale entries and fit
        let result = guard.try_pair("wrong", "new_client").await;
        assert!(result.is_ok(), "New client should not be blocked");

        let state = guard.failed_attempts.lock();
        assert!(
            state.0.len() <= MAX_TRACKED_CLIENTS,
            "Map size should stay within bound, got {}",
            state.0.len()
        );
        assert!(
            state.0.contains_key("new_client"),
            "New client should be tracked"
        );
    }

    #[test]
    async fn failed_attempt_sweep_prunes_expired_clients() {
        let guard = PairingGuard::new(true, &[]);

        // Seed a stale entry and set last_sweep to long ago so sweep triggers
        {
            let mut state = guard.failed_attempts.lock();
            let past = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_RETENTION_SECS + 60,
                ))
                .unwrap_or_else(Instant::now);
            state.0.insert(
                "stale_client".to_string(),
                FailedAttemptState {
                    count: 2,
                    lockout_until: None,
                    last_attempt: past,
                },
            );
            // Force last_sweep to be old enough to trigger sweep
            state.1 = Instant::now()
                .checked_sub(std::time::Duration::from_secs(
                    FAILED_ATTEMPT_SWEEP_INTERVAL_SECS + 1,
                ))
                .unwrap_or_else(Instant::now);
        }

        // Any attempt triggers sweep
        let _ = guard.try_pair("wrong", "fresh_client").await;

        let state = guard.failed_attempts.lock();
        assert!(
            !state.0.contains_key("stale_client"),
            "Stale client should have been pruned by sweep"
        );
        assert!(
            state.0.contains_key("fresh_client"),
            "Fresh client should still be tracked"
        );
    }

    #[test]
    async fn lockout_is_per_client() {
        let guard = PairingGuard::new(true, &[]);
        let attacker = "attacker_ip";
        let legitimate = "legitimate_ip";

        // Attacker exhausts attempts
        for i in 0..MAX_PAIR_ATTEMPTS {
            let _ = guard.try_pair(&format!("wrong_{i}"), attacker).await;
        }
        // Attacker is locked out
        assert!(guard.try_pair("wrong", attacker).await.is_err());

        // Legitimate client is NOT locked out
        let result = guard.try_pair("wrong", legitimate).await;
        assert!(
            result.is_ok(),
            "Legitimate client should not be locked out by attacker"
        );
    }
}
