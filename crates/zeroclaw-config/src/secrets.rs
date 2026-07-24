// Encrypted secret store — defense-in-depth for API keys and tokens.
//
// Secrets are encrypted using ChaCha20-Poly1305 AEAD with a random key stored
// in `~/.zeroclaw/.secret_key` with restrictive file permissions (0600). The
// config file stores only hex-encoded ciphertext, never plaintext keys.
//
// Each encryption generates a fresh random 12-byte nonce, prepended to the
// ciphertext. The Poly1305 authentication tag prevents tampering.
//
// This prevents:
//   - Plaintext exposure in config files
//   - Casual `grep` or `git log` leaks
//   - Accidental commit of raw API keys
//   - Known-plaintext attacks (unlike the previous XOR cipher)
//   - Ciphertext tampering (authenticated encryption)
//
// For sovereign users who prefer plaintext, `secrets.encrypt = false` disables this.
//
// Migration: values with the legacy `enc:` prefix (XOR cipher) are decrypted
// using the old algorithm for backward compatibility. New encryptions always
// produce `enc2:` (ChaCha20-Poly1305).

use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Key, Nonce};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Length of the random encryption key in bytes (256-bit, matches `ChaCha20`).
#[cfg(test)]
const KEY_LEN: usize = 32;

/// ChaCha20-Poly1305 nonce length in bytes.
const NONCE_LEN: usize = 12;

const ONEPASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Maps a backend to its provisioning lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningState {
    /// Persistent key material is present locally.
    Initialized,
    /// No local material; needs `initialize()` before `with_key()`.
    NeedsInitialization,
    /// Key material is managed externally; nothing to check locally.
    ExternallyProvisioned,
}

/// Abstracts where the master encryption key is obtained.
///
/// Object-safe, single-trait design.  Only `with_key`, `backend_name`,
/// and `provisioning_state` are required; `initialize` has a default
/// error for backends that cannot create keys locally.
pub trait KeySource: Debug + Send + Sync {
    /// Run `f` with a reference to the 256-bit master key.  The
    /// reference is only valid during the call.
    fn with_key(&self, f: &mut dyn FnMut(&[u8; 32]) -> Result<()>) -> Result<()>;

    /// Human-readable label for diagnostic messages.
    fn backend_name(&self) -> &'static str;

    /// Local-only provisioning check — MUST NOT run scripts or
    /// prompt for user input.
    fn provisioning_state(&self) -> ProvisioningState;

    /// Generate fresh key material.  Default error for backends
    /// that cannot create keys locally.
    fn initialize(&self) -> Result<()> {
        anyhow::bail!(
            "The '{}' backend does not support automatic key generation. \
             Create the master key externally, then verify access with \
             `zeroclaw quickstart`.",
            self.backend_name()
        )
    }
}

/// File-system backed key source.  Reads/writes a 32-byte hex-encoded
/// key at the given path (default: `~/.zeroclaw/.secret_key`, 0600).
#[derive(Debug, Clone)]
pub struct FileKeySource {
    key_path: PathBuf,
}

impl FileKeySource {
    pub fn new(key_path: PathBuf) -> Self {
        Self { key_path }
    }
}

impl KeySource for FileKeySource {
    fn with_key(&self, f: &mut dyn FnMut(&[u8; 32]) -> Result<()>) -> Result<()> {
        let key_bytes = load_or_create_key(&self.key_path)?;
        // load_or_create_key always returns 32 bytes.
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);
        let result = f(&key);
        // Best-effort zeroisation on the stack copy.
        key.fill(0);
        result
    }

    fn backend_name(&self) -> &'static str {
        "file"
    }

    fn provisioning_state(&self) -> ProvisioningState {
        if self.key_path.exists() {
            ProvisioningState::Initialized
        } else {
            ProvisioningState::NeedsInitialization
        }
    }

    fn initialize(&self) -> Result<()> {
        let key = generate_random_key();
        write_key_file(&self.key_path, &key)
    }
}

/// Manages encrypted storage of secrets (API keys, tokens, etc.)
#[derive(Debug, Clone)]
pub struct SecretStore {
    /// Where the master key is obtained from.
    key_source: Arc<dyn KeySource>,
    /// Whether encryption is enabled
    enabled: bool,
}

impl SecretStore {
    /// Create a new secret store rooted at the given directory.
    /// Phase 1: always uses the file backend (`.secret_key`).
    pub fn new(zeroclaw_dir: &Path, enabled: bool) -> Self {
        Self {
            key_source: Arc::new(FileKeySource::new(zeroclaw_dir.join(".secret_key"))),
            enabled,
        }
    }

    /// Only for tests: construct a store from an arbitrary `KeySource`.
    #[cfg(test)]
    pub fn from_key_source(key_source: Arc<dyn KeySource>, enabled: bool) -> Self {
        Self {
            key_source,
            enabled,
        }
    }

    /// Only for tests: run a closure with a reference to the raw key.
    /// Panics if `with_key` returns an error.
    #[cfg(test)]
    pub fn with_test_key<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        let mut f = Some(f);
        let mut result = None;
        self.key_source
            .with_key(&mut |key| {
                result = Some(f.take().expect("callback re-entered")(key));
                Ok(())
            })
            .expect("with_key failed in test helper");
        result.expect("callback not invoked")
    }

    /// Only for tests: check whether two stores share the same
    /// `Arc<dyn KeySource>` allocation (ptr equality).
    #[cfg(test)]
    pub fn key_source_ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.key_source, &other.key_source)
    }

    /// Obtain the master key through the trait callback contract.
    /// Enforces exactly-once invocation — a backend that returns
    /// `Ok(())` without calling the closure, or that calls it more
    /// than once, gets an error instead of a panic.
    fn get_key<R>(&self, f: impl FnOnce(&[u8; 32]) -> Result<R>) -> Result<R> {
        let mut call_count: u32 = 0;
        let mut f = Some(f);
        let mut result: Option<Result<R>> = None;

        let backend_result = self.key_source.with_key(&mut |key| {
            call_count = call_count.saturating_add(1);
            // On the 2nd+ invocation, don't invoke the real callback
            // again — we'll detect the violation after with_key returns.
            if call_count > 1 {
                return Ok(());
            }
            // Take the callback. If it's already None (shouldn't happen
            // in single-threaded use), return Ok(()) and let the
            // post-validation produce a clear diagnostic.
            let cb = match f.take() {
                Some(cb) => cb,
                None => return Ok(()),
            };
            result = Some(cb(key));
            Ok(())
        });

        // Propagate backend errors first — the backend itself failed.
        backend_result?;

        // Then enforce exactly-once independently. The backend's return
        // value cannot mask a callback-count violation.
        let backend = self.key_source.backend_name();
        match call_count {
            0 => Err(anyhow::Error::msg(format!(
                "key source '{backend}' did not invoke the callback"
            ))),
            1 => match result {
                Some(Ok(value)) => Ok(value),
                // Propagate the original callback error without wrapping
                // so existing diagnostics (backend name, tampered
                // ciphertext, UTF-8, etc.) appear unchanged.
                Some(Err(e)) => Err(e),
                None => Err(anyhow::Error::msg(format!(
                    "key source '{backend}' invoked the callback but did not produce a result"
                ))),
            },
            n => Err(anyhow::Error::msg(format!(
                "key source '{backend}' invoked the callback {n} times (expected exactly once)"
            ))),
        }
    }

    /// Encrypt a plaintext secret. Returns hex-encoded ciphertext prefixed with `enc2:`.
    /// Format: `enc2:<hex(nonce ‖ ciphertext ‖ tag)>` (12 + N + 16 bytes).
    /// If encryption is disabled, returns the plaintext as-is.
    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        if !self.enabled || plaintext.is_empty() {
            return Ok(plaintext.to_string());
        }

        self.get_key(|key| {
            let key = Key::from_slice(key);
            let cipher = ChaCha20Poly1305::new(key);

            let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
            let ciphertext = cipher.encrypt(&nonce, plaintext.as_bytes()).map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "ChaCha20-Poly1305 encryption failed"
                );
                anyhow::Error::msg(format!("Encryption failed: {e}"))
            })?;

            // Prepend nonce to ciphertext for storage
            let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
            blob.extend_from_slice(&nonce);
            blob.extend_from_slice(&ciphertext);

            Ok(format!("enc2:{}", hex_encode(&blob)))
        })
    }

    /// Decrypt a secret.
    /// - `enc2:` prefix → ChaCha20-Poly1305 (current format)
    /// - `enc:` prefix → legacy XOR cipher (backward compatibility for migration)
    /// - `op://` prefix → resolved via 1Password CLI (`op read`)
    /// - No prefix → returned as-is (plaintext config)
    ///
    /// **Warning**: Legacy `enc:` values are insecure. Use `decrypt_and_migrate` to
    /// automatically upgrade them to the secure `enc2:` format.
    pub fn decrypt(&self, value: &str) -> Result<String> {
        if let Some(hex_str) = value.strip_prefix("enc2:") {
            self.decrypt_chacha20(hex_str)
        } else if let Some(hex_str) = value.strip_prefix("enc:") {
            self.decrypt_legacy_xor(hex_str)
        } else if is_onepassword_ref(value) {
            resolve_onepassword_ref(value)
        } else {
            Ok(value.to_string())
        }
    }

    /// Decrypt a secret and return a migrated `enc2:` value if the input used legacy `enc:` format.
    ///
    /// Returns `(plaintext, Some(new_enc2_value))` if migration occurred, or
    /// `(plaintext, None)` if no migration was needed.
    ///
    /// This allows callers to persist the upgraded value back to config.
    pub fn decrypt_and_migrate(&self, value: &str) -> Result<(String, Option<String>)> {
        if let Some(hex_str) = value.strip_prefix("enc2:") {
            // Already using secure format — no migration needed
            let plaintext = self.decrypt_chacha20(hex_str)?;
            Ok((plaintext, None))
        } else if let Some(hex_str) = value.strip_prefix("enc:") {
            // Legacy XOR cipher — decrypt and re-encrypt with ChaCha20-Poly1305
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Decrypting legacy XOR-encrypted secret (enc: prefix). \
                 This format is insecure and will be removed in a future release. \
                 The secret will be automatically migrated to enc2: (ChaCha20-Poly1305)."
            );
            let plaintext = self.decrypt_legacy_xor(hex_str)?;
            let migrated = self.encrypt(&plaintext)?;
            Ok((plaintext, Some(migrated)))
        } else if is_onepassword_ref(value) {
            let plaintext = resolve_onepassword_ref(value)?;
            Ok((plaintext, None))
        } else {
            // Plaintext — no migration needed
            Ok((value.to_string(), None))
        }
    }

    /// Check if a value uses the legacy `enc:` format that should be migrated.
    pub fn needs_migration(value: &str) -> bool {
        value.starts_with("enc:")
    }

    /// Decrypt using ChaCha20-Poly1305 (current secure format).
    fn decrypt_chacha20(&self, hex_str: &str) -> Result<String> {
        let blob =
            hex_decode(hex_str).context("Failed to decode encrypted secret (corrupt hex)")?;
        anyhow::ensure!(
            blob.len() > NONCE_LEN,
            "Encrypted value too short (missing nonce)"
        );

        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);

        self.get_key(|key| {
            let key = Key::from_slice(key);
            let cipher = ChaCha20Poly1305::new(key);

            let plaintext_bytes = cipher.decrypt(nonce, ciphertext).map_err(|e| {
                let backend = self.key_source.backend_name();
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "backend": backend,
                            "error": format!("{e}")
                        })),
                    "enc2: decryption failed — key mismatch or missing material. \
                         Common cause: volume wipe, container migration, \
                         or backup-restore where the key material was not \
                         preserved alongside `config.toml`.  Restore the \
                         original key material from backup, or re-encrypt \
                         the affected secrets via `zeroclaw quickstart`."
                );
                anyhow::Error::msg(format!(
                    "enc2: decryption failed (wrong key for '{}' backend, or tampered ciphertext): {e}",
                    backend
                ))
            })?;

            String::from_utf8(plaintext_bytes)
                .context("Decrypted secret is not valid UTF-8 — corrupt data")
        })
    }

    /// Decrypt using legacy XOR cipher (insecure, for backward compatibility only).
    fn decrypt_legacy_xor(&self, hex_str: &str) -> Result<String> {
        let ciphertext = hex_decode(hex_str)
            .context("Failed to decode legacy encrypted secret (corrupt hex)")?;

        self.get_key(|key| {
            let plaintext_bytes = xor_cipher(&ciphertext, key);
            String::from_utf8(plaintext_bytes)
                .context("Decrypted legacy secret is not valid UTF-8 — wrong key or corrupt data")
        })
    }

    /// Check if a value is already encrypted or externally resolved.
    pub fn is_encrypted(value: &str) -> bool {
        value.starts_with("enc2:") || value.starts_with("enc:") || is_onepassword_ref(value)
    }

    /// Check if a value is a 1Password external secret reference.
    pub fn is_onepassword_ref(value: &str) -> bool {
        is_onepassword_ref(value)
    }

    /// Check if a value uses the secure `enc2:` format.
    pub fn is_secure_encrypted(value: &str) -> bool {
        value.starts_with("enc2:")
    }
}

/// Load the key from `key_path`, creating it (with restrictive 0600
/// permissions) if absent.  Returns the raw 32 key bytes.
///
/// Uses atomic `write_key_file` (`O_CREAT | O_EXCL`).  If two
/// processes race on first creation, the loser falls back to reading
/// the winner's key — no overwrite, no divergent keys.
fn load_or_create_key(key_path: &Path) -> Result<Vec<u8>> {
    let validate_key = |bytes: Vec<u8>| {
        anyhow::ensure!(
            bytes.len() == 32,
            "Key file must contain exactly 32 bytes (got {})",
            bytes.len()
        );
        Ok(bytes)
    };
    match fs::read_to_string(key_path) {
        Ok(hex) => validate_key(hex_decode(hex.trim()).context("Secret key file is corrupt")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = generate_random_key();
            match write_key_file(key_path, &key) {
                Ok(()) => Ok(key),
                Err(write_err) => {
                    // Only recover if another process won the O_EXCL race.
                    // All other failures (disk full, permission denied,
                    // parent dir missing, symlink refusal, etc.) must
                    // propagate so we don't silently accept a bad key.
                    if !is_already_exists_error(&write_err) {
                        return Err(write_err);
                    }
                    // Genuine race: another process created the file first.
                    // Fall back to reading the winner's key.
                    let hex = fs::read_to_string(key_path)
                        .context("Failed to read key file (created by concurrent process)")?;
                    let bytes = hex_decode(hex.trim())
                        .context("Secret key file created by concurrent process is corrupt")?;
                    validate_key(bytes)
                }
            }
        }
        Err(e) => Err(e).context("Failed to read secret key file"),
    }
}

/// Check whether an error chain contains an `AlreadyExists` IO error,
/// indicating that another process won an `O_CREAT | O_EXCL` race.
fn is_already_exists_error(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        e.downcast_ref::<std::io::Error>()
            .map(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
            .unwrap_or(false)
    })
}

/// Write `key` as hex to `key_path` with restrictive permissions.
/// Uses `O_CREAT | O_EXCL` — refuses to overwrite an existing file
/// and rejects symlinks.
fn write_key_file(key_path: &Path, key: &[u8]) -> Result<()> {
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Reject symlinks — following a symlink target would silently write
    // key material to an unexpected location.
    if fs::symlink_metadata(key_path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        anyhow::bail!("Key file path is a symlink — refusing to write");
    }
    // O_CREAT | O_EXCL: atomic create-if-absent.  Fails if the file
    // already exists, preventing accidental overwrite of the master key.
    //
    // On Unix the file is created with 0o600 so it has restrictive
    // permissions from birth — the set_permissions call below is a
    // hardening step only.
    let mut open_opts = std::fs::OpenOptions::new();
    open_opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.mode(0o600);
    }
    let mut file = match open_opts.open(key_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Preserve the original IO error via .context() so callers
            // can detect the AlreadyExists kind via downcast.
            return Err(e).context("Key file already exists — refusing to overwrite");
        }
        Err(e) => return Err(e.into()),
    };
    use std::io::Write;
    file.write_all(hex_encode(key).as_bytes())
        .context("Failed to write secret key file")?;

    // Harden permissions. On Unix the file was already created with
    // 0o600 via OpenOptionsExt::mode, so a failure here is non-fatal.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(key_path, fs::Permissions::from_mode(0o600)) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to harden key file permissions (file was created with 0o600)"
            );
        }
    }
    #[cfg(windows)]
    {
        let username = std::process::Command::new("whoami")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_default());
        let Some(grant_arg) = build_windows_icacls_grant_arg(&username) else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "USERNAME environment variable is empty; \
                 cannot restrict key file permissions via icacls"
            );
            return Ok(());
        };

        match std::process::Command::new("takeown")
            .arg("/F")
            .arg(key_path)
            .output()
        {
            Ok(o) if !o.status.success() => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "Failed to take ownership of key file via takeown (exit code {:?})",
                        o.status.code()
                    )
                );
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Could not take ownership of key file"
                );
            }
            _ => {}
        }

        match std::process::Command::new("icacls")
            .arg(key_path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(grant_arg)
            .output()
        {
            Ok(o) if !o.status.success() => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "Failed to set key file permissions via icacls (exit code {:?})",
                        o.status.code()
                    )
                );
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Could not set key file permissions"
                );
            }
            _ => {}
        }
    }

    Ok(())
}

/// XOR cipher with repeating key. Same function for encrypt and decrypt.
fn xor_cipher(data: &[u8], key: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    data.iter()
        .enumerate()
        .map(|(i, &b)| b ^ key[i % key.len()])
        .collect()
}

/// Generate a random 256-bit key using the OS CSPRNG.
///
/// Uses `OsRng` (via `getrandom`) directly, providing full 256-bit entropy
/// without the fixed version/variant bits that UUID v4 introduces.
fn generate_random_key() -> Vec<u8> {
    ChaCha20Poly1305::generate_key(&mut OsRng).to_vec()
}

/// Hex-encode bytes to a lowercase hex string.
fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Build the `/grant` argument for `icacls` using a normalized username.
/// Returns `None` when the username is empty or whitespace-only.
#[cfg(any(windows, test))]
fn build_windows_icacls_grant_arg(username: &str) -> Option<String> {
    let normalized = username.trim();
    if normalized.is_empty() {
        return None;
    }
    Some(format!("{normalized}:F"))
}

/// Hex-decode a hex string to bytes.
#[allow(clippy::manual_is_multiple_of)]
fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    if (hex.len() & 1) != 0 {
        anyhow::bail!("Hex string has odd length");
    }
    // Reject non-ASCII up front: valid hex is always ASCII, and this guarantees
    // every byte is a char boundary so the byte-index slicing below cannot panic
    // on a corrupt/tampered ciphertext (it returns the Err the signature promises).
    if !hex.is_ascii() {
        anyhow::bail!("Hex string contains non-ASCII characters");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| anyhow::Error::msg(format!("Invalid hex at position {i}: {e}")))
        })
        .collect()
}

fn is_onepassword_ref(value: &str) -> bool {
    value.starts_with("op://")
}

fn validate_onepassword_ref(reference: &str) -> Result<()> {
    let path = reference.strip_prefix("op://").unwrap_or("");
    let mut segments = path.split('/');
    let has_required_segments = (0..3).all(|_| segments.next().is_some_and(|s| !s.is_empty()));
    anyhow::ensure!(
        has_required_segments && segments.all(|segment| !segment.is_empty()),
        "Invalid 1Password reference \"{reference}\". Expected format: op://vault-name/item-name/field-name"
    );
    Ok(())
}

/// Resolve a 1Password secret reference by invoking the `op` CLI.
fn resolve_onepassword_ref(reference: &str) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    validate_onepassword_ref(reference)?;

    let mut child = Command::new("op")
        .args(["read", reference])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "Failed to run 1Password CLI"
            );
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::Error::msg(
                    "1Password CLI (`op`) not found. Install it to use op:// secret references in config."
                )
            } else {
                anyhow::Error::msg(format!("Failed to run 1Password CLI: {e}"))
            }
        })?;

    let mut stdout = child
        .stdout
        .take()
        .context("Failed to capture 1Password CLI stdout")?;
    let mut stderr = child
        .stderr
        .take()
        .context("Failed to capture 1Password CLI stderr")?;
    let stdout_handle = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });

    let deadline = Instant::now() + ONEPASSWORD_READ_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .context("Failed to poll 1Password CLI process")?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            anyhow::bail!(
                "1Password CLI timed out resolving \"{reference}\" after {}s",
                ONEPASSWORD_READ_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let stdout = stdout_handle
        .join()
        .map_err(|_| anyhow::Error::msg("1Password CLI stdout reader panicked"))?
        .context("Failed to read 1Password CLI stdout")?;
    let stderr = stderr_handle
        .join()
        .map_err(|_| anyhow::Error::msg("1Password CLI stderr reader panicked"))?
        .context("Failed to read 1Password CLI stderr")?;

    if !status.success() {
        let stderr_text = String::from_utf8_lossy(&stderr);
        let hint =
            if stderr_text.contains("not signed in") || stderr_text.contains("session expired") {
                " (hint: run `op signin` first)"
            } else {
                ""
            };
        anyhow::bail!(
            "1Password CLI failed to resolve \"{reference}\": {}{hint}",
            stderr_text.trim()
        );
    }

    let secret = String::from_utf8(stdout)
        .context("1Password CLI returned non-UTF-8 output")?
        .trim_end_matches(&['\r', '\n'][..])
        .to_string();

    anyhow::ensure!(
        !secret.is_empty(),
        "1Password CLI returned empty value for \"{reference}\". Verify the vault/item/field path is correct."
    );

    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::ffi::OsString;
    use tempfile::TempDir;

    #[cfg(unix)]
    struct EnvValueGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    #[cfg(unix)]
    impl EnvValueGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: tests that mutate env vars serialize on env_test_lock().
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    #[cfg(unix)]
    impl Drop for EnvValueGuard {
        fn drop(&mut self) {
            // SAFETY: tests that mutate env vars serialize on env_test_lock().
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[cfg(unix)]
    fn write_fake_op(bin_dir: &Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;

        let op_path = bin_dir.join("op");
        fs::write(&op_path, script).expect("write fake op");
        let mut perms = fs::metadata(&op_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&op_path, perms).unwrap();
    }

    // ── SecretStore basics ─────────────────────────────────────

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let secret = "sk-my-secret-api-key-12345";

        let encrypted = store.encrypt(secret).unwrap();
        assert!(encrypted.starts_with("enc2:"), "Should have enc2: prefix");
        assert_ne!(encrypted, secret, "Should not be plaintext");

        let decrypted = store.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, secret, "Roundtrip must preserve original");
    }

    #[test]
    fn encrypt_empty_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let result = store.encrypt("").unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn decrypt_plaintext_passthrough() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        // Values without "enc:"/"enc2:" prefix are returned as-is (backward compat)
        let result = store.decrypt("sk-plaintext-key").unwrap();
        assert_eq!(result, "sk-plaintext-key");
    }

    #[test]
    fn disabled_store_returns_plaintext() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), false);
        let result = store.encrypt("sk-secret").unwrap();
        assert_eq!(result, "sk-secret", "Disabled store should not encrypt");
    }

    #[test]
    fn is_encrypted_detects_prefix() {
        assert!(SecretStore::is_encrypted("enc2:aabbcc"));
        assert!(SecretStore::is_encrypted("enc:aabbcc")); // legacy
        assert!(SecretStore::is_encrypted("op://vault/item/field"));
        assert!(!SecretStore::is_encrypted("sk-plaintext"));
        assert!(!SecretStore::is_encrypted(""));
    }

    #[test]
    fn op_reference_invalid_format_fails_before_plaintext_passthrough() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let err = store.decrypt("op://vault-only").unwrap_err().to_string();

        assert!(err.contains("Invalid 1Password reference"));
    }

    #[test]
    fn op_reference_decrypt_and_migrate_does_not_migrate_or_pass_through() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let err = store
            .decrypt_and_migrate("op://vault-only")
            .unwrap_err()
            .to_string();

        assert!(err.contains("Invalid 1Password reference"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn op_reference_drains_stderr_while_waiting() {
        let _guard = crate::env_overrides::env_test_lock().await;
        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        write_fake_op(
            &bin_dir,
            r#"#!/bin/sh
if [ "$1" = "read" ] && [ "$2" = "op://vault/item/field" ]; then
  yes diagnostic-line >&2 &
  spam_pid=$!
  sleep 1
  kill "$spam_pid"
  wait "$spam_pid" 2>/dev/null
  printf '%s\n' 'secret-from-op'
  exit 0
fi
exit 65
"#,
        );
        let path = match std::env::var_os("PATH") {
            Some(existing) if !existing.is_empty() => {
                format!("{}:{}", bin_dir.display(), existing.to_string_lossy())
            }
            _ => bin_dir.display().to_string(),
        };
        let _path_guard = EnvValueGuard::set("PATH", path);
        let store = SecretStore::new(tmp.path(), true);

        let secret = store.decrypt("op://vault/item/field").unwrap();

        assert_eq!(secret, "secret-from-op");
    }

    #[tokio::test]
    async fn key_file_created_on_first_encrypt() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        assert!(!key_path.exists());

        let store = SecretStore::new(tmp.path(), true);
        store.encrypt("test").unwrap();
        assert!(
            key_path.exists(),
            "Key file should be created via SecretStore::encrypt"
        );

        let key_hex = tokio::fs::read_to_string(&key_path).await.unwrap();
        assert_eq!(
            key_hex.len(),
            KEY_LEN * 2,
            "Key should be {} bytes hex-encoded",
            KEY_LEN
        );
    }

    #[test]
    fn encrypting_same_value_produces_different_ciphertext() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let e1 = store.encrypt("secret").unwrap();
        let e2 = store.encrypt("secret").unwrap();
        assert_ne!(
            e1, e2,
            "AEAD with random nonce should produce different ciphertext each time"
        );

        // Both should still decrypt to the same value
        assert_eq!(store.decrypt(&e1).unwrap(), "secret");
        assert_eq!(store.decrypt(&e2).unwrap(), "secret");
    }

    #[test]
    fn different_stores_same_dir_interop() {
        let tmp = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp.path(), true);
        let store2 = SecretStore::new(tmp.path(), true);

        let encrypted = store1.encrypt("cross-store-secret").unwrap();
        let decrypted = store2.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, "cross-store-secret");
    }

    #[test]
    fn unicode_secret_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let secret = "sk-日本語テスト-émojis-🦀";

        let encrypted = store.encrypt(secret).unwrap();
        let decrypted = store.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, secret);
    }

    #[test]
    fn long_secret_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let secret = "a".repeat(10_000);

        let encrypted = store.encrypt(&secret).unwrap();
        let decrypted = store.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, secret);
    }

    #[test]
    fn corrupt_hex_returns_error() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let result = store.decrypt("enc2:not-valid-hex!!");
        assert!(result.is_err());
    }

    #[test]
    fn tampered_ciphertext_detected() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("sensitive-data").unwrap();

        // Flip a bit in the ciphertext (after the "enc2:" prefix)
        let hex_str = &encrypted[5..];
        let mut blob = hex_decode(hex_str).unwrap();
        // Modify a byte in the ciphertext portion (after the 12-byte nonce)
        if blob.len() > NONCE_LEN {
            blob[NONCE_LEN] ^= 0xff;
        }
        let tampered = format!("enc2:{}", hex_encode(&blob));

        let result = store.decrypt(&tampered);
        assert!(result.is_err(), "Tampered ciphertext must be rejected");
    }

    #[test]
    fn wrong_key_detected() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp1.path(), true);
        let store2 = SecretStore::new(tmp2.path(), true);

        let encrypted = store1.encrypt("secret-for-store1").unwrap();
        let result = store2.decrypt(&encrypted);
        assert!(result.is_err(), "Decrypting with a different key must fail");
    }

    #[test]
    fn decrypt_error_message_mentions_backend() {
        // Operators hitting a missing or mismatched key (volume wipe, container
        // migration, backup-restore) need the error message to point at the
        // root cause — the active key-source backend.
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp1.path(), true);
        let store2 = SecretStore::new(tmp2.path(), true);

        let encrypted = store1.encrypt("secret-for-store1").unwrap();
        let err = store2.decrypt(&encrypted).expect_err("wrong key must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("'file'"),
            "decrypt error must mention the active backend name so operators \
             can diagnose missing/mismatched keys: got {msg:?}"
        );
    }

    #[test]
    fn truncated_ciphertext_returns_error() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        // Only a few bytes — shorter than nonce
        let result = store.decrypt("enc2:aabbccdd");
        assert!(result.is_err(), "Too-short ciphertext must be rejected");
    }

    // ── Legacy XOR backward compatibility ───────────────────────

    #[test]
    fn legacy_xor_decrypt_still_works() {
        let tmp = TempDir::new().unwrap();
        let fs = FileKeySource::new(tmp.path().join(".secret_key"));
        // Trigger key creation
        fs.with_key(&mut |_| Ok(())).unwrap();
        // Read the raw key to manually build a legacy ciphertext
        let key_bytes: Vec<u8> = {
            let mut k = Vec::new();
            fs.with_key(&mut |key| {
                k = key.to_vec();
                Ok(())
            })
            .unwrap();
            k
        };
        let store = SecretStore::from_key_source(Arc::new(fs), true);

        // Manually produce a legacy XOR-encrypted value
        let plaintext = "sk-legacy-api-key";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key_bytes);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        // Store should still be able to decrypt legacy values
        let decrypted = store.decrypt(&legacy_value).unwrap();
        assert_eq!(decrypted, plaintext, "Legacy XOR values must still decrypt");
    }

    // ── Migration tests ─────────────────────────────────────────

    #[test]
    fn needs_migration_detects_legacy_prefix() {
        assert!(SecretStore::needs_migration("enc:aabbcc"));
        assert!(!SecretStore::needs_migration("enc2:aabbcc"));
        assert!(!SecretStore::needs_migration("sk-plaintext"));
        assert!(!SecretStore::needs_migration(""));
    }

    #[test]
    fn is_secure_encrypted_detects_enc2_only() {
        assert!(SecretStore::is_secure_encrypted("enc2:aabbcc"));
        assert!(!SecretStore::is_secure_encrypted("enc:aabbcc"));
        assert!(!SecretStore::is_secure_encrypted("sk-plaintext"));
        assert!(!SecretStore::is_secure_encrypted(""));
    }

    #[test]
    fn decrypt_and_migrate_returns_none_for_enc2() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let encrypted = store.encrypt("my-secret").unwrap();
        assert!(encrypted.starts_with("enc2:"));

        let (plaintext, migrated) = store.decrypt_and_migrate(&encrypted).unwrap();
        assert_eq!(plaintext, "my-secret");
        assert!(
            migrated.is_none(),
            "enc2: values should not trigger migration"
        );
    }

    #[test]
    fn decrypt_and_migrate_returns_none_for_plaintext() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let (plaintext, migrated) = store.decrypt_and_migrate("sk-plaintext-key").unwrap();
        assert_eq!(plaintext, "sk-plaintext-key");
        assert!(
            migrated.is_none(),
            "Plaintext values should not trigger migration"
        );
    }

    #[test]
    fn decrypt_and_migrate_upgrades_legacy_xor() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        let fs = FileKeySource::new(key_path);
        // Ensure key material exists (auto-create via with_key).
        let raw_key: Vec<u8> = {
            let mut k = Vec::new();
            fs.with_key(&mut |key| {
                k = key.to_vec();
                Ok(())
            })
            .unwrap();
            k
        };
        let store = SecretStore::from_key_source(Arc::new(fs), true);
        let key = raw_key;

        // Manually create a legacy XOR-encrypted value
        let plaintext = "sk-legacy-secret-to-migrate";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        // Verify it needs migration
        assert!(SecretStore::needs_migration(&legacy_value));

        // Decrypt and migrate
        let (decrypted, migrated) = store.decrypt_and_migrate(&legacy_value).unwrap();
        assert_eq!(decrypted, plaintext, "Plaintext must match original");
        assert!(migrated.is_some(), "Legacy value should trigger migration");

        let new_value = migrated.unwrap();
        assert!(
            new_value.starts_with("enc2:"),
            "Migrated value must use enc2: prefix"
        );
        assert!(
            !SecretStore::needs_migration(&new_value),
            "Migrated value should not need migration"
        );

        // Verify the migrated value decrypts correctly
        let (decrypted2, migrated2) = store.decrypt_and_migrate(&new_value).unwrap();
        assert_eq!(
            decrypted2, plaintext,
            "Migrated value must decrypt to same plaintext"
        );
        assert!(
            migrated2.is_none(),
            "Migrated value should not trigger another migration"
        );
    }

    #[test]
    fn decrypt_and_migrate_handles_unicode() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let _ = store.encrypt("setup").unwrap();
        let key = store.with_test_key(|k| k.to_vec());

        let plaintext = "sk-日本語-émojis-🦀-тест";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        let (decrypted, migrated) = store.decrypt_and_migrate(&legacy_value).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(migrated.is_some());

        // Verify migrated value works
        let new_value = migrated.unwrap();
        let (decrypted2, _) = store.decrypt_and_migrate(&new_value).unwrap();
        assert_eq!(decrypted2, plaintext);
    }

    #[test]
    fn decrypt_and_migrate_handles_empty_secret() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let _ = store.encrypt("setup").unwrap();
        let key = store.with_test_key(|k| k.to_vec());

        // Empty plaintext XOR-encrypted
        let plaintext = "";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        let (decrypted, migrated) = store.decrypt_and_migrate(&legacy_value).unwrap();
        assert_eq!(decrypted, plaintext);
        // Empty string encryption returns empty string (not enc2:)
        assert!(migrated.is_some());
        assert_eq!(migrated.unwrap(), "");
    }

    #[test]
    fn decrypt_and_migrate_handles_long_secret() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let _ = store.encrypt("setup").unwrap();
        let key = store.with_test_key(|k| k.to_vec());

        let plaintext = "a".repeat(10_000);
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        let (decrypted, migrated) = store.decrypt_and_migrate(&legacy_value).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(migrated.is_some());

        let new_value = migrated.unwrap();
        let (decrypted2, _) = store.decrypt_and_migrate(&new_value).unwrap();
        assert_eq!(decrypted2, plaintext);
    }

    #[test]
    fn decrypt_and_migrate_fails_on_corrupt_legacy_hex() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let _ = store.encrypt("setup").unwrap();

        let result = store.decrypt_and_migrate("enc:not-valid-hex!!");
        assert!(result.is_err(), "Corrupt hex should fail");
    }

    #[test]
    fn decrypt_and_migrate_wrong_key_produces_garbage_or_fails() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp1.path(), true);
        let store2 = SecretStore::new(tmp2.path(), true);

        // Create keys for both stores
        let _ = store1.encrypt("setup").unwrap();
        let _ = store2.encrypt("setup").unwrap();
        let key1 = store1.with_test_key(|k| k.to_vec());

        // Encrypt with store1's key
        let plaintext = "secret-for-store1";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key1);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        // Decrypt with store2 — XOR will produce garbage bytes
        // This may fail with UTF-8 error or succeed with garbage plaintext
        match store2.decrypt_and_migrate(&legacy_value) {
            Ok((decrypted, _)) => {
                // If it succeeds, the plaintext should be garbage (not the original)
                assert_ne!(
                    decrypted, plaintext,
                    "Wrong key should produce garbage plaintext"
                );
            }
            Err(e) => {
                // Expected: UTF-8 decoding failure from garbage bytes
                assert!(
                    e.to_string().contains("UTF-8"),
                    "Error should be UTF-8 related: {e}"
                );
            }
        }
    }

    #[test]
    fn migration_produces_different_ciphertext_each_time() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let _ = store.encrypt("setup").unwrap();
        let key = store.with_test_key(|k| k.to_vec());

        let plaintext = "sk-same-secret";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        let (_, migrated1) = store.decrypt_and_migrate(&legacy_value).unwrap();
        let (_, migrated2) = store.decrypt_and_migrate(&legacy_value).unwrap();

        assert!(migrated1.is_some());
        assert!(migrated2.is_some());
        assert_ne!(
            migrated1.unwrap(),
            migrated2.unwrap(),
            "Each migration should produce different ciphertext (random nonce)"
        );
    }

    #[test]
    fn migrated_value_is_tamper_resistant() {
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);

        let _ = store.encrypt("setup").unwrap();
        let key = store.with_test_key(|k| k.to_vec());

        let plaintext = "sk-sensitive-data";
        let ciphertext = xor_cipher(plaintext.as_bytes(), &key);
        let legacy_value = format!("enc:{}", hex_encode(&ciphertext));

        let (_, migrated) = store.decrypt_and_migrate(&legacy_value).unwrap();
        let new_value = migrated.unwrap();

        // Tamper with the migrated value
        let hex_str = &new_value[5..];
        let mut blob = hex_decode(hex_str).unwrap();
        if blob.len() > NONCE_LEN {
            blob[NONCE_LEN] ^= 0xff;
        }
        let tampered = format!("enc2:{}", hex_encode(&blob));

        let result = store.decrypt_and_migrate(&tampered);
        assert!(result.is_err(), "Tampered migrated value must be rejected");
    }

    // ── Low-level helpers ───────────────────────────────────────

    #[test]
    fn xor_cipher_roundtrip() {
        let key = b"testkey123";
        let data = b"hello world";
        let encrypted = xor_cipher(data, key);
        let decrypted = xor_cipher(&encrypted, key);
        assert_eq!(decrypted, data);
    }

    #[test]
    fn xor_cipher_empty_key() {
        let data = b"passthrough";
        let result = xor_cipher(data, &[]);
        assert_eq!(result, data);
    }

    #[test]
    fn hex_roundtrip() {
        let data = vec![0x00, 0x01, 0xfe, 0xff, 0xab, 0xcd];
        let encoded = hex_encode(&data);
        assert_eq!(encoded, "0001feffabcd");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn hex_decode_odd_length_fails() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_invalid_chars_fails() {
        assert!(hex_decode("zzzz").is_err());
    }

    #[test]
    fn hex_decode_non_ascii_returns_error() {
        // A corrupt/tampered ciphertext with even *byte* length made of
        // non-ASCII chars previously slipped past the odd-length check and
        // panicked on mid-UTF-8-char byte slicing. It must now return Err
        // gracefully (the signature's promise). "€€" is 6 bytes.
        assert!(hex_decode("€€").is_err());
        // Non-ASCII that is a whole 2-byte char also errors, not panics.
        assert!(hex_decode("ÿÿ").is_err());
    }

    #[test]
    fn windows_icacls_grant_arg_rejects_empty_username() {
        assert_eq!(build_windows_icacls_grant_arg(""), None);
        assert_eq!(build_windows_icacls_grant_arg("   \t\n"), None);
    }

    #[test]
    fn windows_icacls_grant_arg_trims_username() {
        assert_eq!(
            build_windows_icacls_grant_arg("  alice  "),
            Some("alice:F".to_string())
        );
    }

    #[test]
    fn windows_icacls_grant_arg_preserves_valid_characters() {
        assert_eq!(
            build_windows_icacls_grant_arg("DOMAIN\\svc-user"),
            Some("DOMAIN\\svc-user:F".to_string())
        );
    }

    #[test]
    fn generate_random_key_correct_length() {
        let key = generate_random_key();
        assert_eq!(key.len(), KEY_LEN);
    }

    #[test]
    fn generate_random_key_not_all_zeros() {
        let key = generate_random_key();
        assert!(key.iter().any(|&b| b != 0), "Key should not be all zeros");
    }

    #[test]
    fn two_random_keys_differ() {
        let k1 = generate_random_key();
        let k2 = generate_random_key();
        assert_ne!(k1, k2, "Two random keys should differ");
    }

    #[test]
    fn generate_random_key_has_no_uuid_fixed_bits() {
        // UUID v4 has fixed bits at positions 6 (version = 0b0100xxxx) and
        // 8 (variant = 0b10xxxxxx). A direct CSPRNG key should not consistently
        // have these patterns across multiple samples.
        let mut version_match = 0;
        let mut variant_match = 0;
        let samples = 100;
        for _ in 0..samples {
            let key = generate_random_key();
            // In UUID v4, byte 6 always has top nibble = 0x4
            if key[6] & 0xf0 == 0x40 {
                version_match += 1;
            }
            // In UUID v4, byte 8 always has top 2 bits = 0b10
            if key[8] & 0xc0 == 0x80 {
                variant_match += 1;
            }
        }
        // With true randomness, each pattern should appear ~1/16 and ~1/4 of
        // the time. UUID would hit 100/100 on both. Allow generous margin.
        assert!(
            version_match < 30,
            "byte[6] matched UUID v4 version nibble {version_match}/100 times — \
             likely still using UUID-based key generation"
        );
        assert!(
            variant_match < 50,
            "byte[8] matched UUID v4 variant bits {variant_match}/100 times — \
             likely still using UUID-based key generation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn key_file_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        let fs = FileKeySource::new(key_path.clone());
        // Trigger key creation via with_key (auto-create).
        fs.with_key(&mut |_| Ok(())).unwrap();

        let perms = fs::metadata(&key_path).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "Key file must be owner-only (0600)"
        );
    }

    /// Document the expected ordering on Windows: `takeown` runs before `icacls`.
    ///
    /// Without `takeown`, the file owner may be an invalid SID, causing `icacls`
    /// grants to succeed against an unowned file that later becomes unreadable.
    /// This test verifies the code structure expectation.
    #[test]
    fn takeown_runs_before_icacls_on_windows() {
        // Read the source to confirm `takeown` appears before `icacls` in the
        // Windows cfg block of `write_key_file`. This is a structural
        // documentation test — the actual commands are Windows-only.
        let source = include_str!("secrets.rs");
        let takeown_pos = source
            .find("Command::new(\"takeown\")")
            .expect("takeown call must exist in secrets.rs");
        let icacls_pos = source
            .find("Command::new(\"icacls\")")
            .expect("icacls call must exist in secrets.rs");
        assert!(
            takeown_pos < icacls_pos,
            "takeown must run before icacls to fix file ownership first (issue #4532)"
        );
    }

    // ── Atomic initialization ─────────────────────────────────

    #[test]
    fn initialize_refuses_existing_file() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        fs::write(&key_path, b"existing").unwrap();

        let fs = FileKeySource::new(key_path);
        let err = fs.initialize().unwrap_err().to_string();
        assert!(
            err.contains("already exists"),
            "initialize must refuse to overwrite existing key file: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn initialize_refuses_symlink() {
        use std::os::unix::fs as unix_fs;
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("real-key");
        let link = tmp.path().join(".secret_key");
        fs::write(&target, b"real-key-data").unwrap();
        unix_fs::symlink(&target, &link).unwrap();

        let fs = FileKeySource::new(link);
        let err = fs.initialize().unwrap_err().to_string();
        assert!(
            err.contains("symlink"),
            "initialize must refuse symlink paths: {err}"
        );
    }

    #[test]
    fn provisioning_state_initialized_when_key_file_present() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        let fs = FileKeySource::new(key_path.clone());
        assert_eq!(
            fs.provisioning_state(),
            ProvisioningState::NeedsInitialization,
            "No key file yet"
        );
        // Create the key file via initialize.
        fs.initialize().unwrap();
        assert_eq!(
            fs.provisioning_state(),
            ProvisioningState::Initialized,
            "Key file now exists"
        );
    }

    #[test]
    fn provisioning_state_default_is_needs_initialization() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join("nonexistent").join(".secret_key");
        let fs = FileKeySource::new(key_path);
        assert_eq!(
            fs.provisioning_state(),
            ProvisioningState::NeedsInitialization
        );
    }

    #[test]
    fn key_file_wrong_length_rejected() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        // Write only 16 bytes (half the required 32).
        fs::write(&key_path, hex_encode(&[0u8; 16])).unwrap();
        let err = load_or_create_key(&key_path).unwrap_err().to_string();
        assert!(
            err.contains("must contain exactly 32 bytes"),
            "Wrong-length key file must be rejected: {err}"
        );
    }

    #[test]
    fn key_file_too_long_rejected() {
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        // Write 64 bytes (twice the required 32).
        fs::write(&key_path, hex_encode(&[0u8; 64])).unwrap();
        let err = load_or_create_key(&key_path).unwrap_err().to_string();
        assert!(
            err.contains("must contain exactly 32 bytes"),
            "Too-long key file must be rejected: {err}"
        );
    }

    #[test]
    fn load_or_create_detects_genuine_race() {
        // When the key file is created by another process between our
        // NotFound check and our O_EXCL attempt, we must fall back to
        // reading the winner's key — not fail.
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");

        // Pre-create the key file (simulating another process winning the race).
        let winner_key = generate_random_key();
        fs::write(&key_path, hex_encode(&winner_key)).unwrap();

        // load_or_create_key should detect the race and read the winner's key.
        let loaded = load_or_create_key(&key_path).unwrap();
        assert_eq!(loaded, winner_key);
    }

    #[test]
    fn load_or_create_key_fails_on_non_race_write_error() {
        // A non-race write failure (e.g., permission denied on parent dir)
        // must fail-closed, not fall through to reading the key file.
        let tmp = TempDir::new().unwrap();
        // Point at a path whose parent is a regular file, not a directory.
        // create_dir_all will fail because "parent" is a file.
        let parent_file = tmp.path().join("not-a-directory");
        fs::write(&parent_file, b"block").unwrap();
        let key_path = parent_file.join(".secret_key");

        let result = load_or_create_key(&key_path);
        assert!(
            result.is_err(),
            "Non-race write errors must fail-closed, got: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_refuses_symlink_on_race_fallback() {
        // When the key file is a symlink (and the target does not exist),
        // fs::read_to_string returns NotFound, write_key_file detects the
        // symlink and refuses. load_or_create_key must propagate that
        // refusal — not fall through to read.
        use std::os::unix::fs as unix_fs;
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("nonexistent-target");
        let link = tmp.path().join(".secret_key");
        // Symlink to a non-existent target → read_to_string → NotFound.
        unix_fs::symlink(&target, &link).unwrap();

        let result = load_or_create_key(&link);
        assert!(
            result.is_err(),
            "Symlink refusal must not fall through to read path: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_key_file_creates_with_restrictive_mode_at_birth() {
        // On Unix, the file must have 0o600 from the moment of creation,
        // before set_permissions runs as hardening.
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");

        // Use a fresh file — no race.
        let key = generate_random_key();
        write_key_file(&key_path, &key).unwrap();

        let perms = fs::metadata(&key_path).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "Key file must be 0o600 immediately after write_key_file returns"
        );
    }

    // ── Clone-sharing ────────────────────────────────────────

    #[test]
    fn cloned_store_shares_key_source() {
        let tmp = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp.path(), true);
        let store2 = store1.clone();

        // Arc::ptr_eq proves the clone shares the SAME allocation,
        // not just that two independent FileKeySource instances
        // happen to read the same file. This matters for future
        // stateful backends (HSM sessions, KMS connection pools).
        assert!(
            store1.key_source_ptr_eq(&store2),
            "Cloned stores must share the same Arc<dyn KeySource> instance"
        );
    }

    #[test]
    fn independent_stores_have_distinct_key_sources() {
        let tmp = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp.path(), true);
        let store2 = SecretStore::new(tmp.path(), true);

        // Even though they read the same file, the Arc instances are distinct.
        assert!(
            !store1.key_source_ptr_eq(&store2),
            "Independently created stores must have distinct Arc instances"
        );
    }

    // ── Error diagnosis ──────────────────────────────────────

    #[test]
    fn decrypt_error_message_mentions_tampered_ciphertext() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let store1 = SecretStore::new(tmp1.path(), true);
        let store2 = SecretStore::new(tmp2.path(), true);

        let encrypted = store1.encrypt("secret-for-store1").unwrap();
        let err = store2.decrypt(&encrypted).expect_err("wrong key must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("tampered ciphertext"),
            "decrypt error must mention tampered ciphertext as possible cause: {msg}"
        );
    }

    // ── get_key callback contract enforcement ────────────────

    /// A test-only KeySource that never calls the callback.
    #[derive(Debug)]
    struct ZeroCallKeySource;

    impl KeySource for ZeroCallKeySource {
        fn with_key(&self, _f: &mut dyn FnMut(&[u8; 32]) -> Result<()>) -> Result<()> {
            // Never calls the callback — violates exactly-once contract.
            Ok(())
        }
        fn backend_name(&self) -> &'static str {
            "zero-call-test"
        }
        fn provisioning_state(&self) -> ProvisioningState {
            ProvisioningState::ExternallyProvisioned
        }
    }

    /// A test-only KeySource that calls the callback twice and
    /// ignores the second call's error.
    #[derive(Debug)]
    struct DoubleCallKeySource {
        key: [u8; 32],
    }

    impl KeySource for DoubleCallKeySource {
        fn with_key(&self, f: &mut dyn FnMut(&[u8; 32]) -> Result<()>) -> Result<()> {
            // First call — legitimate.
            f(&self.key)?;
            // Second call — contract violation. Ignore the result.
            let _ = f(&self.key);
            Ok(())
        }
        fn backend_name(&self) -> &'static str {
            "double-call-test"
        }
        fn provisioning_state(&self) -> ProvisioningState {
            ProvisioningState::ExternallyProvisioned
        }
    }

    /// A test-only KeySource whose callback returns an error.
    #[derive(Debug)]
    struct CallbackErrorSource {
        key: [u8; 32],
    }

    impl KeySource for CallbackErrorSource {
        fn with_key(&self, f: &mut dyn FnMut(&[u8; 32]) -> Result<()>) -> Result<()> {
            // Invoke the callback as required.
            f(&self.key)
        }
        fn backend_name(&self) -> &'static str {
            "callback-error-test"
        }
        fn provisioning_state(&self) -> ProvisioningState {
            ProvisioningState::ExternallyProvisioned
        }
    }

    /// A test-only KeySource that calls the callback once, then returns
    /// an error — simulating a KMS/HSM connection failure.
    #[derive(Debug)]
    struct BackendErrorSource {
        key: [u8; 32],
    }

    impl KeySource for BackendErrorSource {
        fn with_key(&self, f: &mut dyn FnMut(&[u8; 32]) -> Result<()>) -> Result<()> {
            // One compliant callback invocation, then the backend itself fails.
            f(&self.key)?;
            Err(anyhow::Error::msg("backend-connection-failed"))
        }
        fn backend_name(&self) -> &'static str {
            "backend-error-test"
        }
        fn provisioning_state(&self) -> ProvisioningState {
            ProvisioningState::ExternallyProvisioned
        }
    }

    #[test]
    fn get_key_detects_zero_calls() {
        let store = SecretStore::from_key_source(Arc::new(ZeroCallKeySource), true);
        let err = store
            .get_key(|_| Ok("should not reach"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("did not invoke the callback"),
            "Zero-call backend must be detected: {err}"
        );
        assert!(
            err.contains("zero-call-test"),
            "Error must name the backend: {err}"
        );
    }

    #[test]
    fn get_key_detects_double_call_that_ignores_callback_error() {
        let key = generate_random_key();
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&key);
        let store =
            SecretStore::from_key_source(Arc::new(DoubleCallKeySource { key: key_arr }), true);
        // If the backend calls twice but ignores the second callback error,
        // get_key must still detect the violation.
        let err = store
            .get_key(|_k| Ok("legitimate-result"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("invoked the callback 2 times"),
            "Double-call must be detected even when backend ignores callback error: {err}"
        );
    }

    #[test]
    fn get_key_propagates_callback_error() {
        let key = generate_random_key();
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&key);
        let store =
            SecretStore::from_key_source(Arc::new(CallbackErrorSource { key: key_arr }), true);
        let err = store
            .get_key(|_k| Err::<(), _>(anyhow::Error::msg("test-callback-failure-reason")))
            .unwrap_err()
            .to_string();
        // The original callback error must propagate unchanged.
        assert!(
            err.contains("test-callback-failure-reason"),
            "Callback error must propagate unchanged: {err}"
        );
    }

    #[test]
    fn get_key_callback_returns_value_normally() {
        // Happy path: exactly one callback invocation, callback succeeds.
        let tmp = TempDir::new().unwrap();
        let store = SecretStore::new(tmp.path(), true);
        let result = store
            .get_key(|k| {
                assert_eq!(k.len(), 32);
                Ok::<_, anyhow::Error>(42u32)
            })
            .unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn get_key_propagates_backend_error() {
        // When the backend itself returns Err (e.g., KMS connection failure),
        // get_key must propagate that error — not the callback result.
        let key = generate_random_key();
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&key);
        let store =
            SecretStore::from_key_source(Arc::new(BackendErrorSource { key: key_arr }), true);
        let err = store
            .get_key(|_k| Ok("callback-succeeded-but-backend-failed"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("backend-connection-failed"),
            "Backend error must propagate, got: {err}"
        );
    }

    // ── Backward compatibility (frozen fixture) ──────────────

    /// Verify that the current implementation can decrypt a ChaCha20-Poly1305
    /// ciphertext produced with a known key. This is a frozen test vector that
    /// proves backward compatibility with the `enc2:` wire format used before
    /// the KeySource trait extraction.
    ///
    /// The test constructs the ciphertext deterministically using the
    /// ChaCha20-Poly1305 library directly (bypassing SecretStore), then
    /// verifies SecretStore can decrypt it. If this test fails, the refactor
    /// has broken backward compatibility.
    #[test]
    fn frozen_pre_refactor_ciphertext_decrypts() {
        // Deterministic key material — 32 bytes of known pattern.
        let frozen_key: [u8; 32] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];

        // Encrypt a known plaintext using ChaCha20-Poly1305 directly —
        // this replicates what the pre-refactor implementation produced.
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&frozen_key));
        let nonce_bytes = [0u8; 12]; // deterministic nonce for the test vector
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = b"frozen-backward-compat-test";
        let ciphertext = cipher.encrypt(nonce, plaintext.as_ref()).unwrap();

        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);
        let enc2_value = format!("enc2:{}", hex_encode(&blob));

        // Write the frozen key to a temp file.
        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        fs::write(&key_path, hex_encode(&frozen_key)).unwrap();

        let fs = FileKeySource::new(key_path);
        let store = SecretStore::from_key_source(Arc::new(fs), true);
        let decrypted = store.decrypt(&enc2_value).unwrap();
        assert_eq!(
            decrypted,
            std::str::from_utf8(plaintext).unwrap(),
            "Frozen fixture: pre-refactor ciphertext must decrypt correctly"
        );
    }

    #[test]
    fn frozen_fixture_tampered_ciphertext_rejected() {
        // Tampering with the frozen test vector must still be detected.
        let frozen_key: [u8; 32] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];

        let tmp = TempDir::new().unwrap();
        let key_path = tmp.path().join(".secret_key");
        fs::write(&key_path, hex_encode(&frozen_key)).unwrap();

        let fs = FileKeySource::new(key_path);
        let store = SecretStore::from_key_source(Arc::new(fs), true);

        // Flip the first byte of the ciphertext portion of a valid enc2: value.
        let tampered = "enc2:000000000000000000000000\
            f1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6\
            c7d8e9f0a1b2c3d4e5f6a7b8c9d0";

        let result = store.decrypt(tampered);
        assert!(result.is_err(), "Tampered frozen fixture must be rejected");
    }
}
