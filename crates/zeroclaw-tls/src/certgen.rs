//! Certificate generation for ZeroClaw's mutual-TLS transport.
//!
//! Produces a per-daemon CA and the server / client leaf certificates that chain
//! to it, with correct X.509 profiles: the CA is `CA:TRUE, pathlen:0`
//! (`keyCertSign` + `cRLSign`); the server leaf carries `serverAuth` EKU; the
//! client leaf carries `clientAuth` EKU. `notBefore` is backdated a few minutes
//! for clock skew. This backs the secure-by-default auto-generation path (the
//! daemon mints its own CA + server cert on first run) and client-cert issuance.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// A generated certificate + private key, PEM-encoded.
#[derive(Debug, Clone)]
pub struct Pem {
    /// PEM-encoded certificate.
    pub cert_pem: String,
    /// PEM-encoded PKCS#8 private key.
    pub key_pem: String,
}

/// On-disk paths to the daemon's mTLS server materials.
#[derive(Debug, Clone)]
pub struct ServerMaterials {
    pub ca_cert_path: PathBuf,
    pub ca_key_path: PathBuf,
    pub server_cert_path: PathBuf,
    pub server_key_path: PathBuf,
}

const CA_VALIDITY_DAYS: i64 = 3650; // 10 years (stable per-daemon root)
const SERVER_VALIDITY_DAYS: i64 = 825; // ~27 months
const CLIENT_VALIDITY_DAYS: i64 = 30; // matches the cert-TTL decision
const SKEW_BACKDATE: time::Duration = time::Duration::minutes(5);

fn distinguished_name(common_name: &str) -> rcgen::DistinguishedName {
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, common_name);
    dn
}

fn set_validity(params: &mut rcgen::CertificateParams, days: i64) {
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - SKEW_BACKDATE;
    params.not_after = now + time::Duration::days(days);
}

/// CA profile: `CA:TRUE, pathlen:0`, `keyCertSign` + `cRLSign`. Cannot mint sub-CAs.
fn ca_params(common_name: &str) -> Result<rcgen::CertificateParams> {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new())
        .context("building CA certificate params")?;
    p.distinguished_name = distinguished_name(common_name);
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    p.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    set_validity(&mut p, CA_VALIDITY_DAYS);
    Ok(p)
}

/// Server leaf profile: `CA:FALSE`, `serverAuth` EKU, the given SANs.
fn server_params(sans: &[String]) -> Result<rcgen::CertificateParams> {
    let mut p = rcgen::CertificateParams::new(sans.to_vec())
        .context("building server certificate params")?;
    p.is_ca = rcgen::IsCa::NoCa;
    p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    set_validity(&mut p, SERVER_VALIDITY_DAYS);
    Ok(p)
}

/// Client leaf profile: `CA:FALSE`, `clientAuth` EKU only, subject = device id.
fn client_params(subject_common_name: &str) -> Result<rcgen::CertificateParams> {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new())
        .context("building client certificate params")?;
    p.distinguished_name = distinguished_name(subject_common_name);
    p.is_ca = rcgen::IsCa::NoCa;
    p.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    set_validity(&mut p, CLIENT_VALIDITY_DAYS);
    Ok(p)
}

/// Default SANs for an auto-generated server certificate. Suitable for local /
/// pinned access; operators exposing a public hostname should provide their own
/// server certificate (BYO) with the correct SAN.
fn default_server_sans() -> Vec<String> {
    vec!["localhost".to_string(), "127.0.0.1".to_string()]
}

/// Write a public PEM (cert) file.
fn write_public_pem(path: &Path, pem: &str) -> Result<()> {
    std::fs::write(path, pem).with_context(|| format!("writing {}", path.display()))
}

/// Write a private-key PEM file, restricting permissions to `0600` on Unix
/// **before** the bytes are written (no create-then-chmod world-readable window).
pub fn write_private_pem(path: &Path, pem: &str) -> Result<()> {
    write_private_bytes(path, pem.as_bytes())
}

/// Write secret bytes (a PEM, or an encrypted key envelope) at `0600` on Unix
/// before any bytes are written, with no create-then-chmod world-readable window.
fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        // Enforce 0600 even if the file pre-existed with looser permissions.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Create `dir` (and parents), restricting it to `0700` on Unix.
fn create_secure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restricting permissions on {}", dir.display()))?;
    }
    Ok(())
}

/// Reconstruct the rcgen CA issuer (certificate + key) from PEM strings. The CA
/// key PEM is the crown jewel; callers wrap it in [`Zeroizing`] so it is wiped on
/// drop.
fn load_ca_from_pem(
    ca_cert_pem: &str,
    ca_key_pem: &str,
) -> Result<(rcgen::Certificate, rcgen::KeyPair)> {
    let ca_key = rcgen::KeyPair::from_pem(ca_key_pem).context("loading CA key")?;
    let ca_cert = rcgen::CertificateParams::from_ca_cert_pem(ca_cert_pem)
        .context("loading CA certificate")?
        .self_signed(&ca_key)
        .context("reconstructing CA issuer")?;
    Ok((ca_cert, ca_key))
}

/// Reconstruct the CA issuer from on-disk PEM so it can sign new leaves without
/// rotating the CA. The key is decrypted per `protection` (threat A4).
fn load_ca(
    ca_cert_path: &Path,
    ca_key_path: &Path,
    protection: &CaKeyProtection,
) -> Result<(rcgen::Certificate, rcgen::KeyPair)> {
    let ca_key_pem = load_ca_key_pem(ca_key_path, protection)?;
    let ca_cert_pem = std::fs::read_to_string(ca_cert_path)
        .with_context(|| format!("reading CA certificate {}", ca_cert_path.display()))?;
    load_ca_from_pem(&ca_cert_pem, &ca_key_pem)
}

/// Ensure mTLS server materials exist under `dir`.
///
/// The per-daemon CA is the root of trust and is **never silently rotated**: if
/// `ca.crt` and `ca.key` are present they are loaded and reused, and only a
/// missing server leaf is regenerated. A fresh CA is generated only when the CA
/// key is genuinely absent. This survives partial on-disk state (e.g. a deleted
/// or corrupt `server.crt`) without invalidating already-issued client
/// certificates. Private keys are written `0600` and the directory `0700` on
/// Unix. `server_sans` overrides the default SAN set when non-empty.
pub fn ensure_server_materials(dir: &Path, server_sans: &[String]) -> Result<ServerMaterials> {
    ensure_server_materials_protected(dir, server_sans, &CaKeyProtection::None)
}

/// Like [`ensure_server_materials`], but applies `protection` to the CA private
/// key at rest (threat A4). With [`CaKeyProtection::Passphrase`], a freshly
/// generated CA key is written as an encrypted envelope and an existing key is
/// decrypted with the same passphrase on load. `0600` remains the floor in the
/// [`CaKeyProtection::None`] case.
pub fn ensure_server_materials_protected(
    dir: &Path,
    server_sans: &[String],
    protection: &CaKeyProtection,
) -> Result<ServerMaterials> {
    // Serialize generation across concurrent in-process callers (the daemon runs
    // the WSS listener and the enrollment endpoint as separate tasks that both
    // call this on first boot). Without it, two tasks could interleave the CA
    // cert/key writes and produce a mismatched pair. The lock is held only for the
    // brief, idempotent materialization; steady-state callers short-circuit below.
    static GEN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _gen_guard = GEN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let materials = ServerMaterials {
        ca_cert_path: dir.join("ca.crt"),
        ca_key_path: dir.join("ca.key"),
        server_cert_path: dir.join("server.crt"),
        server_key_path: dir.join("server.key"),
    };

    // Resolve the desired server SANs and detect a change vs. what the current
    // leaf was generated with (recorded in `server.sans`). The SAN-change regen is
    // only enforced when the operator explicitly set SANs; the default
    // localhost/127.0.0.1 path keeps the plain reuse-if-present behaviour so
    // existing daemons are untouched.
    let want_sans = if server_sans.is_empty() {
        default_server_sans()
    } else {
        server_sans.to_vec()
    };
    let sans_marker_path = dir.join("server.sans");
    let want_marker = {
        let mut v: Vec<String> = want_sans
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        v.sort();
        v.dedup();
        v.join("\n")
    };
    let sans_changed = !server_sans.is_empty()
        && std::fs::read_to_string(&sans_marker_path).ok().as_deref() != Some(want_marker.as_str());

    if materials.ca_cert_path.exists()
        && materials.ca_key_path.exists()
        && materials.server_cert_path.exists()
        && materials.server_key_path.exists()
        && !sans_changed
    {
        return Ok(materials);
    }

    create_secure_dir(dir)?;

    // The CA is the crown jewel: reuse it whenever it exists, generate only when
    // its key is genuinely absent.
    let (ca_cert, ca_key) = if materials.ca_cert_path.exists() && materials.ca_key_path.exists() {
        load_ca(&materials.ca_cert_path, &materials.ca_key_path, protection)?
    } else {
        let ca_key = rcgen::KeyPair::generate().context("generating CA key")?;
        let ca_cert = ca_params("ZeroClaw WSS CA")?
            .self_signed(&ca_key)
            .context("self-signing CA certificate")?;
        write_public_pem(&materials.ca_cert_path, &ca_cert.pem())?;
        let ca_key_pem = Zeroizing::new(ca_key.serialize_pem());
        write_ca_key(&materials.ca_key_path, ca_key_pem.as_str(), protection)?;
        (ca_cert, ca_key)
    };

    // (Re)generate the server leaf if it is missing or its configured SANs changed.
    // The CA is never touched, so already-issued client certs keep verifying.
    if !materials.server_cert_path.exists() || !materials.server_key_path.exists() || sans_changed {
        let server_key = rcgen::KeyPair::generate().context("generating server key")?;
        let server_cert = server_params(&want_sans)?
            .signed_by(&server_key, &ca_cert, &ca_key)
            .context("signing server certificate")?;
        write_public_pem(&materials.server_cert_path, &server_cert.pem())?;
        write_private_pem(&materials.server_key_path, &server_key.serialize_pem())?;
        if !server_sans.is_empty() {
            // Record the SAN set the current leaf carries so a later config change
            // is detected and triggers a regen.
            std::fs::write(&sans_marker_path, &want_marker)
                .with_context(|| format!("writing {}", sans_marker_path.display()))?;
        }
    }

    Ok(materials)
}

/// Issue a client certificate signed by the CA whose PEM cert + key are given.
/// The returned key is generated fresh (server-side keygen path, e.g. the
/// operator `issue-client-cert` CLI); the subject CN is the device identity.
///
/// The client keypair is **ECDSA P-256** so the same profile can later be backed
/// by a hardware keystore (iOS Secure Enclave / Android Keystore both support
/// P-256; Ed25519 support is uneven). For the certless-client enrollment path the
/// key never leaves the device: see [`sign_csr`].
pub fn issue_client_cert(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    subject_common_name: &str,
) -> Result<Pem> {
    let (ca_cert, ca_key) = load_ca_from_pem(ca_cert_pem, ca_key_pem)?;

    let leaf_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("generating client key")?;
    let leaf = client_params(subject_common_name)?
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .context("signing client certificate")?;

    Ok(Pem {
        cert_pem: leaf.pem(),
        key_pem: leaf_key.serialize_pem(),
    })
}

/// Metadata for a certificate issued from a client-submitted CSR. The private
/// key is deliberately **absent**: it never leaves the requesting device (only
/// the CSR is transmitted). This is what the daemon records in its issued-cert
/// ledger and returns to the client over the enrollment channel.
#[derive(Debug, Clone)]
pub struct IssuedLeaf {
    /// PEM-encoded signed certificate.
    pub cert_pem: String,
    /// SHA-256 fingerprint (lowercase hex) of the DER certificate. Ledger key.
    pub fingerprint: String,
    /// `notBefore`, unix seconds.
    pub not_before: i64,
    /// `notAfter`, unix seconds.
    pub not_after: i64,
}

/// Sign a client-submitted CSR into a `clientAuth`-only leaf bound to `device_id`.
///
/// SECURITY (threat A7 - CSR field injection): the CA reads **only** the CSR's
/// public key. [`rcgen::CertificateSigningRequestParams::from_pem`] verifies the
/// CSR self-signature and rejects unsupported extensions; we then **discard every
/// CSR-requested field** by replacing the parsed params wholesale with the
/// daemon's own client profile ([`client_params`]: subject CN = `device_id`,
/// `clientAuth`-only EKU, `digitalSignature` KU, `CA:FALSE`, `notBefore`
/// backdated). A requester therefore cannot inject a subject, SAN, EKU, or
/// basic-constraints into the issued certificate. The keypair stays on the
/// device; no private key is returned.
pub fn sign_csr(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    device_id: &str,
    csr_pem: &str,
) -> Result<IssuedLeaf> {
    let (ca_cert, ca_key) = load_ca_from_pem(ca_cert_pem, ca_key_pem)?;

    // `from_pem` verifies the CSR self-signature and rejects unsupported
    // extensions; we ignore the requested params entirely below.
    let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem)
        .context("parsing/verifying client CSR")?;

    // Override: stamp the daemon's own client profile, keeping ONLY the CSR's
    // public key. This is the structural defense against subject/SAN/EKU injection.
    let profile = client_params(device_id)?;
    let not_before = profile.not_before.unix_timestamp();
    let not_after = profile.not_after.unix_timestamp();
    csr.params = profile;

    let cert = csr
        .signed_by(&ca_cert, &ca_key)
        .context("signing client CSR")?;
    let fingerprint = crate::cert_sha256_fingerprint(cert.der().as_ref());
    Ok(IssuedLeaf {
        cert_pem: cert.pem(),
        fingerprint,
        not_before,
        not_after,
    })
}

/// Generate a fresh client keypair (ECDSA P-256, hardware-keystore-backable) and
/// a CSR for it. The CSR subject is only a hint; the daemon issuer overrides it
/// with the device id it assigns (so a requester cannot choose its identity, A7).
/// The private key is returned [`Zeroizing`] - the caller persists it locally and
/// the in-memory copy is wiped on drop; it never leaves the device (only the CSR
/// is transmitted). This is the client half of the [`sign_csr`] enrollment flow.
pub fn generate_client_csr(subject_hint: &str) -> Result<(String, Zeroizing<String>)> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("generating client key")?;
    let mut params =
        rcgen::CertificateParams::new(Vec::<String>::new()).context("building CSR params")?;
    params.distinguished_name = distinguished_name(subject_hint);
    let csr = params
        .serialize_request(&key)
        .context("serializing certificate signing request")?;
    let csr_pem = csr.pem().context("encoding CSR as PEM")?;
    Ok((csr_pem, Zeroizing::new(key.serialize_pem())))
}

// --- CA private-key at-rest protection (threat A4) ---------------------------

/// At-rest protection for the daemon CA private key.
///
/// [`CaKeyProtection::None`] is the secure floor: a plaintext PKCS#8 PEM written
/// `0600`. [`CaKeyProtection::Passphrase`] additionally encrypts the key with
/// XChaCha20-Poly1305 under an scrypt-derived key, so the on-disk bytes are
/// useless without the passphrase (a stolen `ca.key` file does not yield the CA).
/// An OS-keystore-backed variant is a documented future seam.
#[derive(Clone)]
pub enum CaKeyProtection {
    /// Plaintext PKCS#8 PEM at `0600` (backwards-compatible default / floor).
    None,
    /// scrypt + XChaCha20-Poly1305 encryption under an operator passphrase.
    Passphrase(Zeroizing<String>),
}

impl CaKeyProtection {
    /// Build a passphrase protection without the caller naming `zeroize`. An
    /// empty passphrase yields [`CaKeyProtection::None`] (the 0600 floor), so a
    /// daemon can source the passphrase from an env var / file and pass it
    /// through unconditionally.
    pub fn passphrase(passphrase: impl Into<String>) -> Self {
        let p = passphrase.into();
        if p.is_empty() {
            CaKeyProtection::None
        } else {
            CaKeyProtection::Passphrase(Zeroizing::new(p))
        }
    }

    /// Source CA-key protection from the environment (the daemon's opt-in
    /// passphrase; threat A4). `ZEROCLAW_CA_PASSPHRASE` takes precedence; otherwise
    /// `ZEROCLAW_CA_PASSPHRASE_FILE` is read. Unset yields [`CaKeyProtection::None`]
    /// (the 0600 floor). Every CA generation + read path uses this so the on-disk
    /// form always matches.
    pub fn from_env() -> Self {
        if let Ok(p) = std::env::var("ZEROCLAW_CA_PASSPHRASE")
            && !p.trim().is_empty()
        {
            return Self::passphrase(p.trim());
        }
        if let Ok(path) = std::env::var("ZEROCLAW_CA_PASSPHRASE_FILE")
            && let Ok(p) = std::fs::read_to_string(&path)
            && !p.trim().is_empty()
        {
            return Self::passphrase(p.trim());
        }
        CaKeyProtection::None
    }
}

/// Magic header identifying the encrypted CA-key envelope:
/// `magic(8) || salt(16) || nonce(24) || ciphertext`.
const CA_KEY_ENC_MAGIC: &[u8; 8] = b"ZCCAKE1\n";
const CA_KEY_SALT_LEN: usize = 16;
const CA_KEY_NONCE_LEN: usize = 24;
// scrypt cost: log_n=15 (~32 MiB), r=8, p=1 - resists offline brute force while
// staying tractable on small daemon hardware (e.g. a Raspberry Pi) at boot.
const SCRYPT_LOG_N: u8 = 15;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;

fn derive_ca_key(passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, 32)
        .map_err(|e| anyhow::Error::msg(format!("scrypt params: {e}")))?;
    let mut key = Zeroizing::new([0u8; 32]);
    scrypt::scrypt(passphrase.as_bytes(), salt, &params, &mut key[..])
        .map_err(|e| anyhow::Error::msg(format!("scrypt derive: {e}")))?;
    Ok(key)
}

/// Returns true if `bytes` is a recognized encrypted CA-key envelope.
fn is_encrypted_ca_key(bytes: &[u8]) -> bool {
    bytes.len() >= CA_KEY_ENC_MAGIC.len() && &bytes[..CA_KEY_ENC_MAGIC.len()] == CA_KEY_ENC_MAGIC
}

fn encrypt_ca_key_pem(pem: &str, passphrase: &str) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};

    let mut salt = [0u8; CA_KEY_SALT_LEN];
    let mut nonce_bytes = [0u8; CA_KEY_NONCE_LEN];
    getrandom::getrandom(&mut salt).map_err(|e| anyhow::Error::msg(format!("salt rng: {e}")))?;
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|e| anyhow::Error::msg(format!("nonce rng: {e}")))?;

    let key = derive_ca_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key[..]));
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, pem.as_bytes())
        .map_err(|e| anyhow::Error::msg(format!("CA key encrypt: {e}")))?;

    let mut out = Vec::with_capacity(
        CA_KEY_ENC_MAGIC.len() + salt.len() + nonce_bytes.len() + ciphertext.len(),
    );
    out.extend_from_slice(CA_KEY_ENC_MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt_ca_key_pem(bytes: &[u8], passphrase: &str) -> Result<Zeroizing<String>> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};

    let header = CA_KEY_ENC_MAGIC.len() + CA_KEY_SALT_LEN + CA_KEY_NONCE_LEN;
    if !is_encrypted_ca_key(bytes) || bytes.len() < header {
        anyhow::bail!("CA key file is not a recognized encrypted envelope");
    }
    let salt = &bytes[CA_KEY_ENC_MAGIC.len()..CA_KEY_ENC_MAGIC.len() + CA_KEY_SALT_LEN];
    let nonce_bytes = &bytes[CA_KEY_ENC_MAGIC.len() + CA_KEY_SALT_LEN..header];
    let ciphertext = &bytes[header..];

    let key = derive_ca_key(passphrase, salt)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key[..]));
    let nonce = XNonce::from_slice(nonce_bytes);
    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        anyhow::Error::msg("CA key decrypt failed (wrong passphrase or corrupt file)")
    })?;
    let s = String::from_utf8(plaintext).context("decrypted CA key is not valid UTF-8")?;
    Ok(Zeroizing::new(s))
}

/// Write the CA private key honoring `protection`. With a passphrase the bytes on
/// disk are an encrypted envelope; otherwise a plaintext PEM. Either way `0600`.
fn write_ca_key(path: &Path, key_pem: &str, protection: &CaKeyProtection) -> Result<()> {
    match protection {
        CaKeyProtection::None => write_private_bytes(path, key_pem.as_bytes()),
        CaKeyProtection::Passphrase(passphrase) => {
            let envelope = encrypt_ca_key_pem(key_pem, passphrase)?;
            write_private_bytes(path, &envelope)
        }
    }
}

/// Load the CA private-key PEM from disk, decrypting it when the file is an
/// encrypted envelope. Returns a [`Zeroizing`] PEM so the crown-jewel key is
/// wiped on drop. Used by the auto-gen path and by every issuance caller (the
/// `issue-client-cert` CLI and the enrollment endpoint) before signing.
pub fn load_ca_key_pem(path: &Path, protection: &CaKeyProtection) -> Result<Zeroizing<String>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading CA key {}", path.display()))?;
    if is_encrypted_ca_key(&bytes) {
        match protection {
            CaKeyProtection::Passphrase(passphrase) => decrypt_ca_key_pem(&bytes, passphrase),
            CaKeyProtection::None => anyhow::bail!(
                "CA key at {} is encrypted but no passphrase is configured",
                path.display()
            ),
        }
    } else {
        let s = String::from_utf8(bytes).context("CA key is not valid UTF-8")?;
        Ok(Zeroizing::new(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_server_materials_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let m1 = ensure_server_materials(dir.path(), &[]).unwrap();
        assert!(m1.ca_cert_path.exists() && m1.server_key_path.exists());
        let ca1 = std::fs::read_to_string(&m1.ca_cert_path).unwrap();

        // Second call must NOT regenerate (CA is stable / never silently rotated).
        let m2 = ensure_server_materials(dir.path(), &[]).unwrap();
        let ca2 = std::fs::read_to_string(&m2.ca_cert_path).unwrap();
        assert_eq!(ca1, ca2, "CA was regenerated on the second call");
    }

    #[test]
    fn server_sans_are_applied_and_regenerate_only_on_change() {
        use x509_parser::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        let crt = dir.path().join("server.crt");

        let leaf_fp = |p: &std::path::Path| {
            let der = crate::load_certs(&p.to_string_lossy()).unwrap();
            crate::cert_sha256_fingerprint(der[0].as_ref())
        };
        let sans_of = |p: &std::path::Path| -> std::collections::BTreeSet<String> {
            let der = crate::load_certs(&p.to_string_lossy()).unwrap();
            let (_, cert) = X509Certificate::from_der(der[0].as_ref()).unwrap();
            let mut out = std::collections::BTreeSet::new();
            if let Ok(Some(ext)) = cert.subject_alternative_name() {
                for gn in ext.value.general_names.iter() {
                    match gn {
                        GeneralName::DNSName(d) => {
                            out.insert(format!("dns:{}", d.to_ascii_lowercase()));
                        }
                        GeneralName::IPAddress(b) => {
                            let ip = match b.len() {
                                4 => {
                                    let a: [u8; 4] = (*b).try_into().unwrap();
                                    std::net::IpAddr::from(a).to_string()
                                }
                                16 => {
                                    let a: [u8; 16] = (*b).try_into().unwrap();
                                    std::net::IpAddr::from(a).to_string()
                                }
                                _ => continue,
                            };
                            out.insert(format!("ip:{ip}"));
                        }
                        _ => {}
                    }
                }
            }
            out
        };

        // localhost / 127.0.0.1 are passed explicitly by the daemon; custom SANs
        // (a hostname + an IP) ride alongside.
        let base: Vec<String> = vec![
            "localhost".into(),
            "127.0.0.1".into(),
            "zero.example".into(),
            "10.1.2.3".into(),
        ];
        ensure_server_materials(dir.path(), &base).unwrap();
        let s1 = sans_of(&crt);
        assert!(s1.contains("dns:zero.example"), "got {s1:?}");
        assert!(s1.contains("ip:10.1.2.3"), "got {s1:?}");
        assert!(s1.contains("dns:localhost"), "got {s1:?}");
        let fp1 = leaf_fp(&crt);

        // Unchanged SANs: the leaf is reused (no regeneration).
        ensure_server_materials(dir.path(), &base).unwrap();
        assert_eq!(
            fp1,
            leaf_fp(&crt),
            "leaf regenerated despite unchanged SANs"
        );

        // Added SAN: the leaf is regenerated, the new SAN is present, the CA is
        // never rotated.
        let ca_before = std::fs::read_to_string(dir.path().join("ca.crt")).unwrap();
        let mut more = base.clone();
        more.push("shard".into());
        ensure_server_materials(dir.path(), &more).unwrap();
        assert_ne!(
            fp1,
            leaf_fp(&crt),
            "leaf was not regenerated on a SAN change"
        );
        assert!(sans_of(&crt).contains("dns:shard"));
        assert_eq!(
            ca_before,
            std::fs::read_to_string(dir.path().join("ca.crt")).unwrap(),
            "CA must not rotate when only SANs change"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_keys_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let m = ensure_server_materials(dir.path(), &[]).unwrap();
        let mode = std::fs::metadata(&m.ca_key_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "CA key permissions are {mode:o}, expected 600");
    }

    // --- sign_csr (CSR-only issuance, threat A7) -------------------------

    #[test]
    fn sign_csr_round_trips_and_binds_device_id() {
        let (ca_cert, ca_key) = crate::testing::gen_ca();
        let (csr, _device_key) = crate::testing::gen_client_csr("device-abc");
        let leaf = sign_csr(&ca_cert, &ca_key, "device-abc", &csr).unwrap();
        assert!(leaf.cert_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(leaf.fingerprint.len(), 64);
        assert!(leaf.not_after > leaf.not_before);
        // The bound device id appears in the issued cert; no private key is returned.
        assert!(leaf.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(!leaf.cert_pem.contains("PRIVATE KEY"));
    }

    #[test]
    fn generated_client_csr_is_signable() {
        // The client half (generate_client_csr) round-trips with the daemon half
        // (sign_csr): a freshly generated P-256 CSR signs into a usable leaf.
        let (ca_cert, ca_key) = crate::testing::gen_ca();
        let (csr, key_pem) = generate_client_csr("zerocode").unwrap();
        assert!(csr.contains("CERTIFICATE REQUEST"));
        assert!(key_pem.contains("PRIVATE KEY"));
        let leaf = sign_csr(&ca_cert, &ca_key, "dev-xyz", &csr).unwrap();
        assert!(leaf.cert_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(leaf.fingerprint.len(), 64);
    }

    #[test]
    fn sign_csr_ignores_csr_requested_subject_san_and_eku() {
        // A7: the CA must read ONLY the CSR public key and discard requested
        // subject/SAN/EKU. The requester asks for an attacker CN + SAN + serverAuth;
        // the issued cert must carry the daemon-stamped device id, not the attacker's.
        let (ca_cert, ca_key) = crate::testing::gen_ca();
        let (csr, _key) = crate::testing::gen_client_csr_injecting(
            "attacker-cn",
            &["evil-injected.example".to_string()],
        );
        let leaf = sign_csr(&ca_cert, &ca_key, "real-device-007", &csr).unwrap();
        // The issued DER must contain the daemon-stamped device id and NONE of
        // the attacker-requested identity fields.
        let der = pem_cert_to_der(&leaf.cert_pem);
        assert!(
            contains_subslice(&der, b"real-device-007"),
            "issued cert must carry the daemon-stamped device id"
        );
        assert!(
            !contains_subslice(&der, b"attacker-cn"),
            "issued cert must NOT carry the CSR-requested subject"
        );
        assert!(
            !contains_subslice(&der, b"evil-injected.example"),
            "issued cert must NOT carry the CSR-requested SAN"
        );
    }

    #[test]
    fn sign_csr_rejects_malformed_or_corrupted_csr() {
        let (ca_cert, ca_key) = crate::testing::gen_ca();
        // Not a PEM at all.
        assert!(sign_csr(&ca_cert, &ca_key, "d", "not a pem").is_err());
        // Well-formed PEM frame, garbage body.
        assert!(
            sign_csr(
                &ca_cert,
                &ca_key,
                "d",
                "-----BEGIN CERTIFICATE REQUEST-----\nZ0JK\n-----END CERTIFICATE REQUEST-----\n"
            )
            .is_err()
        );
        // A valid CSR with a corrupted body must be rejected (parse or
        // self-signature verification fails in rcgen's from_pem; tamper/replay A9).
        let (csr, _key) = crate::testing::gen_client_csr("device-x");
        let corrupted = corrupt_pem_body(&csr);
        assert!(sign_csr(&ca_cert, &ca_key, "device-x", &corrupted).is_err());
    }

    // --- CA-key at-rest encryption (threat A4) ---------------------------

    #[test]
    fn ca_key_encryption_round_trips() {
        let pem = "-----BEGIN PRIVATE KEY-----\nMOCKKEYBYTES\n-----END PRIVATE KEY-----\n";
        let envelope = encrypt_ca_key_pem(pem, "correct horse battery staple").unwrap();
        assert!(is_encrypted_ca_key(&envelope));
        assert!(
            !contains_subslice(&envelope, b"PRIVATE KEY"),
            "encrypted envelope must not contain the plaintext PEM"
        );
        let decrypted = decrypt_ca_key_pem(&envelope, "correct horse battery staple").unwrap();
        assert_eq!(&*decrypted, pem);
    }

    #[test]
    fn ca_key_decrypt_fails_on_wrong_passphrase() {
        let pem = "-----BEGIN PRIVATE KEY-----\nMOCKKEYBYTES\n-----END PRIVATE KEY-----\n";
        let envelope = encrypt_ca_key_pem(pem, "right").unwrap();
        assert!(decrypt_ca_key_pem(&envelope, "wrong").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn protected_materials_encrypt_ca_key_at_rest_and_load_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let protection = CaKeyProtection::Passphrase(Zeroizing::new("s3cret".to_string()));
        let m = ensure_server_materials_protected(dir.path(), &[], &protection).unwrap();

        // On-disk CA key is an encrypted envelope (no plaintext PEM), still 0600.
        let raw = std::fs::read(&m.ca_key_path).unwrap();
        assert!(
            is_encrypted_ca_key(&raw),
            "CA key must be encrypted at rest"
        );
        assert!(!contains_subslice(&raw, b"PRIVATE KEY"));
        let mode = std::fs::metadata(&m.ca_key_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        // The right passphrase loads the key and can sign a client CSR.
        let key_pem = load_ca_key_pem(&m.ca_key_path, &protection).unwrap();
        let ca_cert_pem = std::fs::read_to_string(&m.ca_cert_path).unwrap();
        let (csr, _) = crate::testing::gen_client_csr("dev1");
        assert!(sign_csr(&ca_cert_pem, &key_pem, "dev1", &csr).is_ok());

        // The wrong passphrase (and no passphrase) must fail closed.
        let wrong = CaKeyProtection::Passphrase(Zeroizing::new("nope".to_string()));
        assert!(load_ca_key_pem(&m.ca_key_path, &wrong).is_err());
        assert!(load_ca_key_pem(&m.ca_key_path, &CaKeyProtection::None).is_err());
    }

    // Parse a single certificate PEM into its DER bytes via rustls_pemfile.
    fn pem_cert_to_der(pem: &str) -> Vec<u8> {
        let mut rdr = std::io::BufReader::new(pem.as_bytes());
        rustls_pemfile::certs(&mut rdr)
            .next()
            .expect("a certificate in the PEM")
            .expect("valid certificate DER")
            .as_ref()
            .to_vec()
    }
    fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
        needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
    }
    /// Flip one base64 character in each body line of a PEM so the encoded
    /// structure / signature no longer verifies, while keeping the PEM frame.
    fn corrupt_pem_body(pem: &str) -> String {
        let mut out: String = pem
            .lines()
            .map(|l| {
                if l.starts_with("-----") || l.len() < 6 {
                    l.to_string()
                } else {
                    let mut chars: Vec<char> = l.chars().collect();
                    chars[2] = if chars[2] == 'A' { 'B' } else { 'A' };
                    chars.into_iter().collect()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.push('\n');
        out
    }

    #[test]
    fn ca_is_reused_when_only_server_leaf_is_missing() {
        // A deleted/corrupt server leaf (partial state) must NOT rotate the CA:
        // the crown-jewel key and cert must survive byte-identical.
        let dir = tempfile::tempdir().unwrap();
        let m = ensure_server_materials(dir.path(), &[]).unwrap();
        let ca_crt = std::fs::read(&m.ca_cert_path).unwrap();
        let ca_key = std::fs::read(&m.ca_key_path).unwrap();

        // Delete only the server certificate and regenerate.
        std::fs::remove_file(&m.server_cert_path).unwrap();
        let m2 = ensure_server_materials(dir.path(), &[]).unwrap();

        assert_eq!(
            ca_crt,
            std::fs::read(&m2.ca_cert_path).unwrap(),
            "CA certificate was rotated on partial state"
        );
        assert_eq!(
            ca_key,
            std::fs::read(&m2.ca_key_path).unwrap(),
            "CA key was rotated on partial state"
        );
        assert!(
            m2.server_cert_path.exists(),
            "server leaf was not regenerated"
        );
    }
}
