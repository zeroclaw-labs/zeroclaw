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
        f.write_all(pem.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        // Enforce 0600 even if the file pre-existed with looser permissions.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("restricting permissions on {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, pem).with_context(|| format!("writing {}", path.display()))?;
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

/// Reconstruct the rcgen CA issuer (certificate + key) from on-disk PEM so it can
/// sign new leaves without rotating the CA.
fn load_ca(
    ca_cert_path: &Path,
    ca_key_path: &Path,
) -> Result<(rcgen::Certificate, rcgen::KeyPair)> {
    let ca_key_pem = std::fs::read_to_string(ca_key_path)
        .with_context(|| format!("reading CA key {}", ca_key_path.display()))?;
    let ca_cert_pem = std::fs::read_to_string(ca_cert_path)
        .with_context(|| format!("reading CA certificate {}", ca_cert_path.display()))?;
    let ca_key = rcgen::KeyPair::from_pem(&ca_key_pem).context("loading CA key")?;
    let ca_cert = rcgen::CertificateParams::from_ca_cert_pem(&ca_cert_pem)
        .context("loading CA certificate")?
        .self_signed(&ca_key)
        .context("reconstructing CA issuer")?;
    Ok((ca_cert, ca_key))
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
    let materials = ServerMaterials {
        ca_cert_path: dir.join("ca.crt"),
        ca_key_path: dir.join("ca.key"),
        server_cert_path: dir.join("server.crt"),
        server_key_path: dir.join("server.key"),
    };

    if materials.ca_cert_path.exists()
        && materials.ca_key_path.exists()
        && materials.server_cert_path.exists()
        && materials.server_key_path.exists()
    {
        return Ok(materials);
    }

    create_secure_dir(dir)?;

    // The CA is the crown jewel: reuse it whenever it exists, generate only when
    // its key is genuinely absent.
    let (ca_cert, ca_key) = if materials.ca_cert_path.exists() && materials.ca_key_path.exists() {
        load_ca(&materials.ca_cert_path, &materials.ca_key_path)?
    } else {
        let ca_key = rcgen::KeyPair::generate().context("generating CA key")?;
        let ca_cert = ca_params("ZeroClaw WSS CA")?
            .self_signed(&ca_key)
            .context("self-signing CA certificate")?;
        write_public_pem(&materials.ca_cert_path, &ca_cert.pem())?;
        write_private_pem(&materials.ca_key_path, &ca_key.serialize_pem())?;
        (ca_cert, ca_key)
    };

    // (Re)generate the server leaf only if it is missing.
    if !materials.server_cert_path.exists() || !materials.server_key_path.exists() {
        let server_key = rcgen::KeyPair::generate().context("generating server key")?;
        let sans = if server_sans.is_empty() {
            default_server_sans()
        } else {
            server_sans.to_vec()
        };
        let server_cert = server_params(&sans)?
            .signed_by(&server_key, &ca_cert, &ca_key)
            .context("signing server certificate")?;
        write_public_pem(&materials.server_cert_path, &server_cert.pem())?;
        write_private_pem(&materials.server_key_path, &server_key.serialize_pem())?;
    }

    Ok(materials)
}

/// Issue a client certificate signed by the CA whose PEM cert + key are given.
/// The returned key is generated fresh; the subject CN is the device identity.
pub fn issue_client_cert(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    subject_common_name: &str,
) -> Result<Pem> {
    let ca_key = rcgen::KeyPair::from_pem(ca_key_pem).context("loading CA key")?;
    let ca_cert = rcgen::CertificateParams::from_ca_cert_pem(ca_cert_pem)
        .context("loading CA certificate")?
        .self_signed(&ca_key)
        .context("reconstructing CA issuer")?;

    let leaf_key = rcgen::KeyPair::generate().context("generating client key")?;
    let leaf = client_params(subject_common_name)?
        .signed_by(&leaf_key, &ca_cert, &ca_key)
        .context("signing client certificate")?;

    Ok(Pem {
        cert_pem: leaf.pem(),
        key_pem: leaf_key.serialize_pem(),
    })
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
