// Single-use recovery codes (Finding F2).
//
// Generated at TOTP setup. 10 codes, 8 alphanumeric chars each.
// CSPRNG-generated. Stored as SHA-256 hashes (not plaintext).
// Each code consumed on use. Warn when < threshold remaining.

use chrono::Utc;
use rand::Rng;
use sha2::{Digest, Sha256};

use super::types::RecoveryCodeEntry;

/// Characters used for recovery codes. Ambiguous chars removed (0/O, 1/l/I).
const CHARSET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

/// Generate a set of recovery codes.
/// Returns (plaintext_codes_for_user, hashed_entries_for_storage).
pub fn generate_recovery_codes(
    count: usize,
    length: usize,
) -> (Vec<String>, Vec<RecoveryCodeEntry>) {
    let mut rng = rand::thread_rng();
    let mut plaintext = Vec::with_capacity(count);
    let mut entries = Vec::with_capacity(count);

    for _ in 0..count {
        let code: String = (0..length)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect();

        entries.push(RecoveryCodeEntry {
            code_hash: hash_code(&code),
            used: false,
            used_at: None,
        });
        plaintext.push(code);
    }

    (plaintext, entries)
}

/// Validate a recovery code against stored entries.
/// Returns the index of the matching code if valid, None if invalid or used.
pub fn validate_recovery_code(
    code: &str,
    entries: &[RecoveryCodeEntry],
) -> Option<usize> {
    let normalized = code.trim().to_lowercase();
    let code_hash = hash_code(&normalized);

    entries.iter().position(|e| !e.used && e.code_hash == code_hash)
}

/// Mark a recovery code as consumed.
pub fn consume_recovery_code(entries: &mut [RecoveryCodeEntry], index: usize) {
    if let Some(entry) = entries.get_mut(index) {
        entry.used = true;
        entry.used_at = Some(Utc::now());
    }
}

/// Count remaining (unused) recovery codes.
pub fn remaining_codes(entries: &[RecoveryCodeEntry]) -> usize {
    entries.iter().filter(|e| !e.used).count()
}

/// SHA-256 hash of a recovery code (for storage — never store plaintext).
fn hash_code(code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    hex::encode(hasher.finalize())
}

/// Hex encoding helper (avoiding an extra dependency).
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_correct_count_and_length() {
        let (codes, entries) = generate_recovery_codes(10, 8);
        assert_eq!(codes.len(), 10);
        assert_eq!(entries.len(), 10);
        for code in &codes {
            assert_eq!(code.len(), 8);
        }
    }

    #[test]
    fn all_codes_are_unique() {
        let (codes, _) = generate_recovery_codes(10, 8);
        let mut sorted = codes.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 10, "duplicate recovery codes generated");
    }

    #[test]
    fn code_works_exactly_once() {
        let (codes, mut entries) = generate_recovery_codes(10, 8);
        let code = &codes[0];

        // First use: succeeds
        let idx = validate_recovery_code(code, &entries);
        assert!(idx.is_some());
        consume_recovery_code(&mut entries, idx.unwrap());

        // Second use: fails (consumed)
        let idx2 = validate_recovery_code(code, &entries);
        assert!(idx2.is_none());
    }

    #[test]
    fn wrong_code_rejected() {
        let (_, entries) = generate_recovery_codes(10, 8);
        let idx = validate_recovery_code("zzzzzzzz", &entries);
        assert!(idx.is_none());
    }

    #[test]
    fn remaining_count_decreases() {
        let (codes, mut entries) = generate_recovery_codes(10, 8);
        assert_eq!(remaining_codes(&entries), 10);

        let idx = validate_recovery_code(&codes[0], &entries).unwrap();
        consume_recovery_code(&mut entries, idx);
        assert_eq!(remaining_codes(&entries), 9);
    }

    #[test]
    fn case_insensitive_validation() {
        let (codes, entries) = generate_recovery_codes(10, 8);
        let upper = codes[0].to_uppercase();
        // All codes are lowercase, but validation normalizes
        let idx = validate_recovery_code(&upper, &entries);
        assert!(idx.is_some());
    }

    #[test]
    fn no_ambiguous_chars() {
        let (codes, _) = generate_recovery_codes(100, 8);
        for code in &codes {
            assert!(!code.contains('0'), "contains 0");
            assert!(!code.contains('o'), "contains o");
            assert!(!code.contains('1'), "contains 1");
            assert!(!code.contains('l'), "contains l");
            assert!(!code.contains('i'), "contains i");
        }
    }
}
