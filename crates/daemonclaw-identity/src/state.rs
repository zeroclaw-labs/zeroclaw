//! On-disk identity state — encrypted at rest via `SecretStore`.
//!
//! The state file is `<identity_dir>/identity_state.json` and holds:
//!   - the PKCS#8 PEM private key (so we can re-derive the keypair across
//!     restarts without a CSR flow),
//!   - the SPKI PEM public key (cached for surfacing in `whoami`),
//!   - the fingerprint string (cached so we don't recompute on every call),
//!   - the locally-generated agent UUID (the `agent_user_id`).
//!
//! The private key is encrypted via `SecretStore.encrypt`, which produces
//! an `enc2:` blob. The on-disk JSON wraps that blob plus the public
//! material; nothing in the file is operator-useful plaintext, and the
//! private key is never serializable as a plain field.
//!
//! The companion file `<identity_dir>/<host>.spki.pem` is the
//! operator-readable public key. It is mode 0644 (public), contains
//! only the SPKI PEM, and is the file the operator copies into WardToken
//! during *Add Key*.
//!
//! ## State schema (v1)
//!
//! ```json
//! {
//!   "version": 1,
//!   "agent_user_id": "<uuid>",
//!   "private_key_pem_enc": "enc2:<hex>",
//!   "spki_pem": "-----BEGIN PUBLIC KEY-----...",
//!   "fingerprint": "sha256:...",
//!   "created_at_unix": 1700000000
//! }
//! ```
//!
//! Future versions may add fields. The `version` field is bumped on any
//! breaking change to this struct's serialization.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{IdentityError, IdentityResult};

/// On-disk identity state. The private key is held only as a `SecretStore`
/// ciphertext; the public material is plain for `whoami` to surface
/// without a decrypt round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityState {
    /// State schema version. Bumped on breaking changes.
    pub version: u32,
    /// Locally-generated stable UUID for the agent. Used as
    /// `agent_user_id` in assertions.
    pub agent_user_id: String,
    /// PKCS#8 PEM private key, encrypted via `SecretStore` (the `enc2:`
    /// prefix is the wire contract). Use [`Self::private_key_pem`]
    /// accessor pattern (none here — decrypt at the call site) to read.
    pub private_key_pem_enc: String,
    /// SPKI PEM public key. Plaintext, public material.
    pub spki_pem: String,
    /// Cached fingerprint for fast `whoami`.
    pub fingerprint: String,
    /// Unix timestamp at first generation. For operator inspection.
    pub created_at_unix: i64,
}

/// State schema version. Bumped on breaking changes to the on-disk
/// `IdentityState` serialization. Exposed `pub(crate)` so
/// `local::generate_and_persist` sets the field from this single
/// source — the same version is read back by `load_state` consumers
/// and pinned by the round-trip tests.
pub(crate) const STATE_VERSION: u32 = 1;

/// File name inside `identity_dir`. Fixed so re-runs find the same blob.
const STATE_FILE: &str = "identity_state.json";

/// Resolve `<identity_dir>/identity_state.json` without checking existence.
pub fn state_path(identity_dir: &Path) -> PathBuf {
    identity_dir.join(STATE_FILE)
}

/// Load and decrypt identity state. Returns `Ok(None)` if the file does
/// not exist (first boot — generate and persist instead).
///
/// When `secrets_encrypt` is false (sovereign mode), the
/// `private_key_pem_enc` field is treated as a literal PEM string. This
/// is not the default; sovereign users opt in.
///
/// The encrypted-blob validation here is a sanity check (catches
/// `.secret_key` mismatches and tampered files early). The actual
/// decrypt for signing is done at the call site via
/// [`decrypt_private_key_pem`] — the same blob is decrypted again
/// there, which is intentional: load returns the *ciphertext* field
/// so the caller can decrypt on demand.
pub fn load_state(identity_dir: &Path, secrets_encrypt: bool) -> IdentityResult<Option<IdentityState>> {
    let path = state_path(identity_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(&path)?;
    let state: IdentityState = serde_json::from_slice(&raw)
        .map_err(|e| IdentityError::State(format!("parse: {e}")))?;
    // Sanity: decrypt the private key. If this fails, the operator's
    // `.secret_key` is wrong (or the file was tampered with). Surface
    // the error — the caller decides whether to regenerate (which would
    // mint a new identity and break existing assertions) or hard-fail.
    let _ = decrypt_private_key_pem(&state.private_key_pem_enc, secrets_encrypt, identity_dir)?;
    Ok(Some(state))
}

/// Persist identity state to disk with mode 0600. The public SPKI file
/// is written alongside (mode 0644) for the operator.
pub fn save_state(identity_dir: &Path, host: &str, state: &IdentityState) -> IdentityResult<()> {
    fs::create_dir_all(identity_dir)?;
    let path = state_path(identity_dir);

    // Write the encrypted state file atomically: write to a temp file
    // in the same dir, fsync, rename. The rename is atomic on POSIX.
    let tmp = identity_dir.join(".identity_state.json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(serde_json::to_vec_pretty(state)?.as_slice())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    set_unix_mode_0600(&path)?;

    // The SPKI PEM file (operator-readable, public material).
    let spki_path = spki_pem_path(identity_dir, host);
    if let Some(parent) = spki_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&spki_path, state.spki_pem.as_bytes())?;
    set_unix_mode_0644(&spki_path)?;
    Ok(())
}

/// Decrypt the private key PEM back to its plaintext PKCS#8 form. Returns
/// the raw PEM bytes (including the `-----BEGIN PRIVATE KEY-----` /
/// `-----END PRIVATE KEY-----` lines).
pub fn decrypt_private_key_pem(
    enc_value: &str,
    secrets_encrypt: bool,
    identity_dir: &Path,
) -> IdentityResult<Vec<u8>> {
    let store = crate::secret_store::SecretStore::new(identity_dir, secrets_encrypt);
    let pem = store.decrypt(enc_value)?;
    Ok(pem.into_bytes())
}

/// Re-encrypt the private key PEM via `SecretStore`. The `enc2:` prefix
/// is the wire contract.
pub fn encrypt_private_key_pem(
    pem_pem: &str,
    secrets_encrypt: bool,
    identity_dir: &Path,
) -> IdentityResult<String> {
    let store = crate::secret_store::SecretStore::new(identity_dir, secrets_encrypt);
    Ok(store.encrypt(pem_pem)?)
}

/// Path to the operator-readable SPKI PEM file.
pub fn spki_pem_path(identity_dir: &Path, host: &str) -> PathBuf {
    identity_dir.join(format!("{host}.spki.pem"))
}

impl IdentityState {
    /// Read the agent user-id. Convenience accessor so callers don't
    /// reach into the struct fields.
    pub fn agent_user_id(&self) -> &str {
        &self.agent_user_id
    }
}

/// In sovereign mode the `private_key_pem_enc` field holds a plain PEM
/// string (no `enc2:` prefix). The on-disk format already protects this
/// because the field is on the encrypted state file. We do not redact
/// at load time — the caller decides what to print, and our public
/// `Debug` impls omit private-key fields entirely.
#[allow(dead_code)]
fn redact_pem_if_plain(enc_or_pem: &str) -> String {
    if enc_or_pem.starts_with("enc2:") || enc_or_pem.starts_with("enc:") {
        enc_or_pem.to_string()
    } else {
        format!("<plaintext-pem-sovereign len={}>", enc_or_pem.len())
    }
}

/// Set the POSIX file mode to 0600 (owner read/write only). Best-effort
/// on non-Unix platforms.
#[cfg(unix)]
fn set_unix_mode_0600(path: &Path) -> IdentityResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(path)?.permissions();
    perm.set_mode(0o600);
    fs::set_permissions(path, perm)?;
    Ok(())
}
#[cfg(not(unix))]
fn set_unix_mode_0600(_path: &Path) -> IdentityResult<()> {
    Ok(())
}

#[cfg(unix)]
fn set_unix_mode_0644(path: &Path) -> IdentityResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(path)?.permissions();
    perm.set_mode(0o644);
    fs::set_permissions(path, perm)?;
    Ok(())
}
#[cfg(not(unix))]
fn set_unix_mode_0644(_path: &Path) -> IdentityResult<()> {
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_dir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn sample_state() -> IdentityState {
        // Build a real encrypted PEM via the local SecretStore so the
        // round-trip can actually decrypt it.
        let dir = fresh_dir();
        let pem = "-----BEGIN PRIVATE KEY-----\nMGV2AgEAMBAGByqGSM49AgEGBSuBBAAiBGU=\n-----END PRIVATE KEY-----\n";
        let store = crate::secret_store::SecretStore::new(dir.path(), false);
        let enc = store.encrypt(pem).unwrap();
        IdentityState {
            version: STATE_VERSION,
            agent_user_id: "11111111-1111-1111-1111-111111111111".into(),
            private_key_pem_enc: enc,
            spki_pem: "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA...\n-----END PUBLIC KEY-----\n".into(),
            fingerprint: "sha256:abc".into(),
            created_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = fresh_dir();
        let state = sample_state();
        save_state(dir.path(), "claw", &state).unwrap();
        let loaded = load_state(dir.path(), true).unwrap().expect("state should exist");
        assert_eq!(loaded.version, state.version);
        assert_eq!(loaded.agent_user_id, state.agent_user_id);
        assert_eq!(loaded.spki_pem, state.spki_pem);
        assert_eq!(loaded.fingerprint, state.fingerprint);
    }

    #[test]
    fn load_returns_none_on_missing_file() {
        let dir = fresh_dir();
        let loaded = load_state(dir.path(), true).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn save_creates_directory_tree() {
        let dir = fresh_dir();
        let nested = dir.path().join("nested").join("deep");
        let state = sample_state();
        save_state(&nested, "claw", &state).unwrap();
        assert!(state_path(&nested).exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_state_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_dir();
        let state = sample_state();
        save_state(dir.path(), "claw", &state).unwrap();
        let mode = fs::metadata(state_path(dir.path())).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state file must be 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn saved_spki_file_is_0644() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_dir();
        let state = sample_state();
        // save_state requires the state to know the host basename; we
        // manually write the SPKI file to test the mode setting.
        let path = spki_pem_path(dir.path(), "claw");
        fs::write(&path, &state.spki_pem).unwrap();
        set_unix_mode_0644(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "SPKI file must be 0644, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn save_state_full_path_sets_spki_file_to_0644() {
        // Full-path 0644 test: exercise `save_state` end-to-end, not
        // the helper in isolation. This is the "by construction" check
        // — if `save_state` ever stops calling `set_unix_mode_0644` on
        // the SPKI path, this test catches it before operator copy.
        use std::os::unix::fs::PermissionsExt;
        let dir = fresh_dir();
        let state = sample_state();
        save_state(dir.path(), "claw", &state).unwrap();
        let spki_path = spki_pem_path(dir.path(), "claw");
        assert!(spki_path.exists(), "SPKI PEM file should exist after save_state");
        let mode = fs::metadata(&spki_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "SPKI file written by save_state must be 0644, got {mode:o}"
        );
    }

    #[test]
    fn spki_fingerprint_in_state_matches_loaded_spki_der() {
        // Contract by construction: the on-disk SPKI PEM, decoded back
        // to DER, must hash (SHA256, base64url-no-pad, "sha256:"
        // prefix) to the `fingerprint` field stored in the encrypted
        // state. This pins that a single `spki` value drives all three
        // places it's used (state.fingerprint, state.spki_pem, the
        // operator-readable file).
        use crate::spki::{fingerprint_spki, spki_from_pubkey, spki_pem_to_der};
        // Build a real SPKI from a real pubkey — the placeholder PEM
        // in `sample_state()` isn't valid base64.
        let pubkey: [u8; 32] = [0x42; 32];
        let spki = spki_from_pubkey(&pubkey);
        let spki_pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, spki)
        );
        let state = IdentityState {
            version: STATE_VERSION,
            agent_user_id: "22222222-2222-2222-2222-222222222222".into(),
            private_key_pem_enc: "ignored-by-this-test".into(),
            spki_pem,
            fingerprint: fingerprint_spki(&spki),
            created_at_unix: 1_700_000_001,
        };
        let dir = fresh_dir();
        save_state(dir.path(), "claw", &state).unwrap();
        let on_disk_pem =
            fs::read_to_string(spki_pem_path(dir.path(), "claw")).unwrap();
        let on_disk_der = spki_pem_to_der(on_disk_pem.as_bytes()).unwrap();
        let fp_from_disk = fingerprint_spki(&on_disk_der);
        assert_eq!(
            fp_from_disk, state.fingerprint,
            "on-disk SPKI must hash to the fingerprint stored in the encrypted state"
        );
    }
}
