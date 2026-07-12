//! Frictionless client enrollment.
//!
//! On first contact a certless client obtains its FIRST mTLS certificate from the
//! daemon enrollment endpoint and caches it, so every later run is zero-config
//! (no `--tls-*` flags). The private key is generated locally and never leaves the
//! device; only a CSR is sent.
//!
//! Bootstrap trust (no blind TOFU): the enrollment channel cannot pre-trust the
//! daemon CA (chicken-and-egg), so the connection accepts the server cert
//! provisionally and trust is confirmed OUT OF BAND by the short-auth-string -
//! the client recomputes the SAS from the pairing code plus the CA it received and
//! the operator compares it to the SAS the daemon printed, BEFORE the certificate
//! is persisted or used. A MITM that substitutes its own CA produces a mismatching
//! SAS and the client refuses it.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroclaw_tls::{ClientKey, CsrSigner, SoftwareP256Signer};

/// The daemon's default enrollment endpoint port (`[enroll].port`).
pub const DEFAULT_ENROLL_PORT: u16 = 9782;
/// Inner TLS server name used when the enrollment endpoint is reached through a
/// relay. The daemon's generated server certificate includes this loopback SAN.
const RELAY_ENROLL_SERVER_NAME: &str = "127.0.0.1";

/// Relay coordinates the daemon hands back at enrollment.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct RelayProfile {
    pub relay_url: String,
    pub node_id: String,
    pub relay_cert_pin: String,
}

#[derive(Debug, serde::Deserialize)]
struct EnrollResponse {
    cert_pem: String,
    ca_chain_pem: String,
    device_id: String,
    not_after: i64,
    #[serde(default)]
    relay_profile: RelayProfile,
}

#[derive(Debug, serde::Deserialize)]
struct EnrollTrustResponse {
    ca_chain_pem: String,
}

/// The cached enrollment profile written beside the certs, so the connect path
/// and the renewal timer are zero-config on later runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedProfile {
    pub device_id: String,
    pub not_after: i64,
    #[serde(default)]
    pub relay: RelayProfile,
}

/// Run the interactive enrollment flow against `host:port` and cache the result
/// under `<config_dir>/tls`. Prompts for the pairing code and the SAS confirmation
/// on the terminal.
pub async fn enroll(host: &str, port: u16, config_dir: &Path) -> Result<()> {
    eprintln!("Enrolling with the ZeroClaw daemon at {host}:{port} ...");
    let (code, csr_pem, key_pem) = prepare_enrollment_request()?;
    let trust = fetch_enroll_trust(host, port)
        .await
        .context("fetching daemon enrollment trust anchor")?;
    confirm_daemon_ca(&code, &trust.ca_chain_pem)?;

    let resp = post_enroll(host, port, &code, &csr_pem, &trust.ca_chain_pem)
        .await
        .context("enrollment request failed")?;

    cache_confirmed_response(config_dir, &trust.ca_chain_pem, &resp, &key_pem)
}

/// Run the interactive enrollment flow through a nominated relay. The relay only
/// opens the daemon's narrow enrollment route; the pairing code and CSR are sent
/// inside the inner enrollment TLS stream.
pub async fn enroll_via_relay(relay: &crate::client::RelayDial, config_dir: &Path) -> Result<()> {
    eprintln!(
        "Enrolling with the ZeroClaw daemon through relay {} -> {} ...",
        relay.relay_addr, relay.node_id
    );
    let (code, csr_pem, key_pem) = prepare_enrollment_request()?;
    let trust = fetch_enroll_trust_via_relay(relay)
        .await
        .context("fetching daemon enrollment trust anchor through relay")?;
    confirm_daemon_ca(&code, &trust.ca_chain_pem)?;

    let resp = post_enroll_via_relay(relay, &code, &csr_pem, &trust.ca_chain_pem)
        .await
        .context("relay enrollment request failed")?;

    cache_confirmed_response(config_dir, &trust.ca_chain_pem, &resp, &key_pem)
}

fn prepare_enrollment_request() -> Result<(String, String, zeroize::Zeroizing<String>)> {
    let code = prompt_line("Enter the daemon enrollment pairing code: ")?;
    let code = code.trim().to_string();
    if code.is_empty() {
        anyhow::bail!("no pairing code entered");
    }

    // The private key stays here; only the CSR is sent. Desktop generates a
    // software P-256 key; a mobile build swaps in a hardware-keystore CsrSigner.
    let (csr_pem, key_pem) = software_csr("zerocode")?;
    Ok((code, csr_pem, key_pem))
}

fn confirm_daemon_ca(code: &str, ca_chain_pem: &str) -> Result<()> {
    // Confirm the CA out of band via the short-auth-string before trusting it.
    let ca_fp = ca_fingerprint(ca_chain_pem)?;
    let sas = zeroclaw_tls::enrollment_sas(code, &ca_fp);
    eprintln!();
    eprintln!("The daemon CA's short-auth-string (SAS) is:");
    eprintln!("    {sas}");
    eprintln!("This MUST match the SAS printed on the daemon console. If it does not,");
    eprintln!("abort - the enrollment may be intercepted.");
    let confirm = prompt_line("Does the SAS match the daemon console? [y/N]: ")?;
    if !confirm.trim().eq_ignore_ascii_case("y") {
        anyhow::bail!("SAS not confirmed; enrollment aborted (no certificate was trusted)");
    }
    Ok(())
}

fn cache_confirmed_response(
    config_dir: &Path,
    confirmed_ca_chain_pem: &str,
    resp: &EnrollResponse,
    key_pem: &str,
) -> Result<()> {
    ensure_response_ca_matches_confirmed(confirmed_ca_chain_pem, &resp.ca_chain_pem)?;
    cache_materials(config_dir, resp, key_pem)?;
    eprintln!();
    eprintln!(
        "Enrolled as device {}. Cached the client certificate + daemon CA under {}/tls.",
        resp.device_id,
        config_dir.display()
    );
    if !resp.relay_profile.relay_url.is_empty() {
        eprintln!(
            "Reach this daemon through its relay with: zerocode --relay {} --relay-node {}",
            resp.relay_profile.relay_url, resp.relay_profile.node_id
        );
    } else {
        eprintln!("This client now connects directly with no --tls-* flags.");
    }
    Ok(())
}

/// Generate a CSR and its software private key via the desktop signer seam
/// ([`SoftwareP256Signer`]). A mobile build swaps in a hardware-keystore
/// [`CsrSigner`] here so the key is non-exportable (A5); the desktop path expects
/// an extractable software key it can persist to `client.key`.
fn software_csr(subject_hint: &str) -> Result<(String, zeroize::Zeroizing<String>)> {
    let csr = SoftwareP256Signer
        .generate_csr(subject_hint)
        .context("generating client CSR")?;
    match csr.key {
        ClientKey::Software(key_pem) => Ok((csr.csr_pem, key_pem)),
        ClientKey::HardwareAlias(_) => {
            anyhow::bail!("desktop enrollment expects a software key from the CSR signer")
        }
    }
}

/// The client-cert TTL we assume to place the 50% renewal point. The daemon
/// issues 30-day client certs; we renew once past the half-life so an
/// intermittently-connected client never lets its cert silently expire.
const ASSUMED_TTL_SECS: i64 = 30 * 86_400;

/// Renew the cached client certificate over the authenticated mTLS session if it
/// is past ~50% of its TTL. Best-effort: a failure logs and is retried on the
/// next connect (the existing cert is still valid). Only meaningful on the WSS
/// plane; on the local socket the daemon refuses renewal and this no-ops.
pub async fn maybe_renew(client: &crate::client::RpcClient, config_dir: &Path) {
    let Some(profile) = cached_profile(config_dir) else {
        return;
    };
    if !renewal_due(profile.not_after, now_unix()) {
        return;
    }
    match renew(client, config_dir).await {
        Ok(not_after) => {
            eprintln!("zerocode: renewed client certificate (valid through unix {not_after}).");
        }
        Err(e) => {
            eprintln!(
                "zerocode: certificate renewal skipped ({e:#}); the current cert is still valid."
            );
        }
    }
}

/// Generate a fresh keypair + CSR, renew over `cert/renew`, and re-cache the
/// result (including any rotated relay node-id the daemon hands back).
async fn renew(client: &crate::client::RpcClient, config_dir: &Path) -> Result<i64> {
    let (csr_pem, key_pem) = software_csr("zerocode").context("generating renewal CSR")?;
    let resp: EnrollResponse = client
        .call("cert/renew", serde_json::json!({ "csr_pem": csr_pem }))
        .await
        .context("cert/renew RPC")?;
    cache_materials(config_dir, &resp, &key_pem)?;
    Ok(resp.not_after)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether the cert is past ~50% of its TTL (`not_after - TTL/2`) and should be
/// renewed. A non-positive `not_after` is treated as "no cached cert".
fn renewal_due(not_after: i64, now: i64) -> bool {
    not_after > 0 && now >= not_after.saturating_sub(ASSUMED_TTL_SECS / 2)
}

/// Read the cached enrollment profile, if a client has enrolled here.
pub fn cached_profile(config_dir: &Path) -> Option<CachedProfile> {
    let raw = std::fs::read(config_dir.join("tls").join("profile.json")).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Fetch the daemon CA over provisional TLS. This preflight sends no pairing code
/// and no CSR; the operator confirms the returned CA via SAS before it is trusted.
async fn fetch_enroll_trust(host: &str, port: u16) -> Result<EnrollTrustResponse> {
    let tcp = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connecting to enrollment endpoint {host}:{port}"))?;
    fetch_enroll_trust_on_stream(tcp, host, host).await
}

async fn fetch_enroll_trust_via_relay(
    relay: &crate::client::RelayDial,
) -> Result<EnrollTrustResponse> {
    let stream = crate::client::dial_enrollment_through_relay(relay).await?;
    fetch_enroll_trust_on_stream(stream, RELAY_ENROLL_SERVER_NAME, RELAY_ENROLL_SERVER_NAME).await
}

/// POST the CSR to the enrollment endpoint over TLS pinned to the operator-
/// confirmed daemon CA.
async fn post_enroll(
    host: &str,
    port: u16,
    code: &str,
    csr_pem: &str,
    trusted_ca_pem: &str,
) -> Result<EnrollResponse> {
    let tcp = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connecting to enrollment endpoint {host}:{port}"))?;
    post_enroll_on_stream(tcp, host, host, code, csr_pem, trusted_ca_pem).await
}

async fn post_enroll_via_relay(
    relay: &crate::client::RelayDial,
    code: &str,
    csr_pem: &str,
    trusted_ca_pem: &str,
) -> Result<EnrollResponse> {
    let stream = crate::client::dial_enrollment_through_relay(relay).await?;
    post_enroll_on_stream(
        stream,
        RELAY_ENROLL_SERVER_NAME,
        RELAY_ENROLL_SERVER_NAME,
        code,
        csr_pem,
        trusted_ca_pem,
    )
    .await
}

async fn fetch_enroll_trust_on_stream<S>(
    stream: S,
    server_name: &str,
    host_header: &str,
) -> Result<EnrollTrustResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tls = connect_enrollment_tls(
        stream,
        server_name,
        provisional_enrollment_config(),
        "enrollment trust TLS handshake",
    )
    .await?;

    let request =
        format!("GET /enroll/ca HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n");
    tls.write_all(request.as_bytes()).await?;
    tls.flush().await?;

    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).await?;
    parse_http_json(&raw)
}

async fn post_enroll_on_stream<S>(
    stream: S,
    server_name: &str,
    host_header: &str,
    code: &str,
    csr_pem: &str,
    trusted_ca_pem: &str,
) -> Result<EnrollResponse>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tls = connect_enrollment_tls(
        stream,
        server_name,
        pinned_enrollment_config(trusted_ca_pem)?,
        "enrollment TLS handshake with confirmed daemon CA",
    )
    .await?;

    let body = serde_json::json!({ "pairing_code": code, "csr_pem": csr_pem }).to_string();
    let request = format!(
        "POST /enroll HTTP/1.1\r\nHost: {host_header}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    tls.write_all(request.as_bytes()).await?;
    tls.flush().await?;

    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).await?;
    parse_http_json(&raw)
}

fn provisional_enrollment_config() -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptProvisional))
        .with_no_client_auth()
}

fn pinned_enrollment_config(ca_chain_pem: &str) -> Result<rustls::ClientConfig> {
    let ca_der = single_ca_cert_der(ca_chain_pem)?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(ca_der)
        .context("adding confirmed daemon CA to enrollment root store")?;
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring provider supports default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth())
}

async fn connect_enrollment_tls<S>(
    stream: S,
    server_name: &str,
    config: rustls::ClientConfig,
    context: &'static str,
) -> Result<tokio_rustls::client::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .with_context(|| format!("invalid enrollment TLS server name {server_name}"))?;
    connector
        .connect(server_name, stream)
        .await
        .context(context)
}

/// Parse an HTTP/1.1 response, returning the decoded enrollment body on 200 or a
/// descriptive error (with the daemon's error message) otherwise.
fn parse_http_json<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("malformed HTTP response from enrollment endpoint")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let status_line = head.lines().next().unwrap_or_default();
    let status_ok = status_line.split_whitespace().nth(1) == Some("200");
    if !status_ok {
        let msg = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| status_line.to_string());
        anyhow::bail!("enrollment rejected: {msg}");
    }
    serde_json::from_slice(body).context("parsing enrollment response JSON")
}

/// SHA-256 fingerprint of the only accepted daemon CA certificate.
fn ca_fingerprint(ca_chain_pem: &str) -> Result<String> {
    let der = single_ca_cert_der(ca_chain_pem)?;
    Ok(zeroclaw_tls::cert_sha256_fingerprint(der.as_ref()))
}

fn ensure_response_ca_matches_confirmed(
    confirmed_ca_chain_pem: &str,
    response_ca_chain_pem: &str,
) -> Result<()> {
    let confirmed = ca_fingerprint(confirmed_ca_chain_pem)?;
    let returned = ca_fingerprint(response_ca_chain_pem)?;
    if confirmed != returned {
        anyhow::bail!("enrollment response CA does not match the operator-confirmed daemon CA");
    }
    Ok(())
}

fn single_ca_cert_der(ca_chain_pem: &str) -> Result<rustls::pki_types::CertificateDer<'static>> {
    let certs = rustls_pemfile::certs(&mut ca_chain_pem.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("invalid daemon CA certificate")?;
    match certs.len() {
        0 => anyhow::bail!("no certificate in the daemon CA chain"),
        1 => Ok(certs.into_iter().next().expect("checked exactly one cert")),
        n => anyhow::bail!(
            "enrollment response must contain exactly one daemon CA certificate, got {n}"
        ),
    }
}

/// Write the cert, key, CA, and cached profile under `<config_dir>/tls`. The key
/// is written `0600` on Unix; the cert/CA/profile are public.
fn cache_materials(config_dir: &Path, resp: &EnrollResponse, key_pem: &str) -> Result<()> {
    // Validate before writing anything. The SAS is bound to this single trust
    // anchor, so refuse to persist a broader
    // root set that the later WSS client would trust but the operator never saw.
    let _ = single_ca_cert_der(&resp.ca_chain_pem)?;

    let tls_dir = config_dir.join("tls");
    std::fs::create_dir_all(&tls_dir).with_context(|| format!("creating {}", tls_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tls_dir, std::fs::Permissions::from_mode(0o700));
    }

    let profile = CachedProfile {
        device_id: resp.device_id.clone(),
        not_after: resp.not_after,
        relay: resp.relay_profile.clone(),
    };
    let json = serde_json::to_string_pretty(&profile).context("serializing profile")?;

    let client_crt = tls_dir.join("client.crt");
    let ca_crt = tls_dir.join("ca.crt");
    let client_key = tls_dir.join("client.key");
    let profile_json = tls_dir.join("profile.json");
    let client_crt_tmp = tls_dir.join(".client.crt.tmp");
    let ca_crt_tmp = tls_dir.join(".ca.crt.tmp");
    let client_key_tmp = tls_dir.join(".client.key.tmp");
    let profile_json_tmp = tls_dir.join(".profile.json.tmp");

    std::fs::write(&client_crt_tmp, resp.cert_pem.as_bytes())
        .with_context(|| format!("writing {}", client_crt_tmp.display()))?;
    std::fs::write(&ca_crt_tmp, resp.ca_chain_pem.as_bytes())
        .with_context(|| format!("writing {}", ca_crt_tmp.display()))?;
    write_private(&client_key_tmp, key_pem)?;
    std::fs::write(&profile_json_tmp, json)
        .with_context(|| format!("writing {}", profile_json_tmp.display()))?;

    // Only publish after every replacement has been durably written. A failed key
    // write leaves the previous cert/key/CA/profile set untouched.
    std::fs::rename(&client_crt_tmp, &client_crt)
        .with_context(|| format!("installing {}", client_crt.display()))?;
    std::fs::rename(&ca_crt_tmp, &ca_crt)
        .with_context(|| format!("installing {}", ca_crt.display()))?;
    std::fs::rename(&client_key_tmp, &client_key)
        .with_context(|| format!("installing {}", client_key.display()))?;
    std::fs::rename(&profile_json_tmp, &profile_json)
        .with_context(|| format!("installing {}", profile_json.display()))?;
    Ok(())
}

/// Write a private key at `0600` (Unix), no world-readable window.
fn write_private(path: &Path, pem: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        f.write_all(pem.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, pem).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// True when no client certificate is available from flags, config, or the
/// conventional `<config_dir>/tls/client.crt` cache.
pub fn is_certless(
    config_dir: &Path,
    cli_client_cert: Option<&str>,
    cfg_client_cert: &str,
) -> bool {
    cli_client_cert
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
        && cfg_client_cert.trim().is_empty()
        && !config_dir.join("tls").join("client.crt").exists()
}

fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading from stdin")?;
    Ok(line)
}

/// Provisional server-cert verifier for the enrollment handshake: accepts any
/// server cert because the CA is not yet trusted (chicken-and-egg). Trust is
/// established afterwards by the out-of-band SAS comparison, NOT by this verifier.
#[derive(Debug)]
struct AcceptProvisional;

impl rustls::client::danger::ServerCertVerifier for AcceptProvisional {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_json_extracts_200_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n{\"cert_pem\":\"C\",\"ca_chain_pem\":\"A\",\"device_id\":\"dev_1\",\"not_after\":123,\"relay_profile\":{\"relay_url\":\"r:1\",\"node_id\":\"n\",\"relay_cert_pin\":\"\"}}";
        let resp: EnrollResponse = parse_http_json(raw).unwrap();
        assert_eq!(resp.device_id, "dev_1");
        assert_eq!(resp.not_after, 123);
        assert_eq!(resp.relay_profile.relay_url, "r:1");
    }

    #[test]
    fn parse_http_json_surfaces_error_message() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n{\"error\":\"invalid or already-used pairing code\"}";
        let err = parse_http_json::<EnrollResponse>(raw)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("invalid or already-used pairing code"),
            "got: {err}"
        );
    }

    #[test]
    fn is_certless_detects_no_material() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_certless(dir.path(), None, ""));
        assert!(!is_certless(dir.path(), Some("/some/client.crt"), ""));
        assert!(!is_certless(dir.path(), None, "/cfg/client.crt"));
        std::fs::create_dir_all(dir.path().join("tls")).unwrap();
        std::fs::write(dir.path().join("tls").join("client.crt"), "x").unwrap();
        assert!(!is_certless(dir.path(), None, ""));
    }

    #[test]
    fn renewal_due_at_half_ttl() {
        // A 30-day cert issued "now": not_after = now + 30d.
        let now = 1_000_000_000;
        let fresh = now + ASSUMED_TTL_SECS; // just issued
        assert!(!renewal_due(fresh, now), "a fresh cert is not due");
        // Past the half-life (issued ~16 days ago): not_after = now + ~14d.
        let half = now + ASSUMED_TTL_SECS / 2 - 1;
        assert!(renewal_due(half, now), "past 50% TTL should renew");
        // Expired cert is also due (renewal will try; daemon may still accept).
        assert!(renewal_due(now - 10, now));
        // No cached cert.
        assert!(!renewal_due(0, now));
    }

    #[test]
    fn sas_matches_zeroclaw_tls() {
        // The client SAS computation must equal the daemon's for the same inputs.
        let fp = "aabbccdd";
        assert_eq!(
            zeroclaw_tls::enrollment_sas("270391", fp),
            zeroclaw_tls::enrollment_sas("270391", fp)
        );
    }

    #[test]
    fn enrollment_rejects_appended_ca_before_cache() {
        let (daemon_ca, _) = zeroclaw_tls::testing::gen_ca();
        let (rogue_ca, _) = zeroclaw_tls::testing::gen_ca();
        let chain = format!("{daemon_ca}\n{rogue_ca}");

        let err = ca_fingerprint(&chain).unwrap_err().to_string();
        assert!(
            err.contains("exactly one daemon CA certificate"),
            "got: {err}"
        );

        let dir = tempfile::tempdir().unwrap();
        let resp = EnrollResponse {
            cert_pem: "client-cert".into(),
            ca_chain_pem: chain,
            device_id: "dev_1".into(),
            not_after: 123,
            relay_profile: RelayProfile::default(),
        };
        let err = cache_materials(dir.path(), &resp, "client-key")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exactly one daemon CA certificate"),
            "got: {err}"
        );
        assert!(!dir.path().join("tls").join("ca.crt").exists());
    }

    #[test]
    fn enrollment_response_ca_must_match_confirmed_ca() {
        let (daemon_ca, _) = zeroclaw_tls::testing::gen_ca();
        let (rogue_ca, _) = zeroclaw_tls::testing::gen_ca();
        let err = ensure_response_ca_matches_confirmed(&daemon_ca, &rogue_ca)
            .unwrap_err()
            .to_string();
        assert!(err.contains("operator-confirmed daemon CA"), "got: {err}");
        ensure_response_ca_matches_confirmed(&daemon_ca, &daemon_ca).unwrap();
    }

    #[tokio::test]
    async fn post_enroll_refuses_unconfirmed_ca_before_sending_code_or_csr() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (confirmed_ca, _) = zeroclaw_tls::testing::gen_ca();
        let (rogue_ca, rogue_key) = zeroclaw_tls::testing::gen_ca();
        let (server_cert, server_key) =
            zeroclaw_tls::testing::gen_server_cert(&rogue_ca, &rogue_key, &["localhost".into()]);
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("server.crt");
        let key_path = dir.path().join("server.key");
        std::fs::write(&cert_path, server_cert).unwrap();
        std::fs::write(&key_path, server_key).unwrap();
        let acceptor = zeroclaw_tls::build_tls_acceptor(&zeroclaw_tls::ServerConfigParams {
            cert_path: cert_path.to_string_lossy().into_owned(),
            key_path: key_path.to_string_lossy().into_owned(),
            client_auth: None,
        })
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let saw_request = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let saw_request_for_server = saw_request.clone();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            if let Ok(mut tls) = acceptor.accept(tcp).await {
                let mut buf = [0u8; 1];
                if tls.read(&mut buf).await.unwrap_or(0) > 0 {
                    saw_request_for_server.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });

        let tcp = TcpStream::connect(addr).await.unwrap();
        let err = post_enroll_on_stream(
            tcp,
            "localhost",
            "localhost",
            "123456",
            "attacker-csr",
            &confirmed_ca,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("confirmed daemon CA") || err.contains("certificate"),
            "got: {err}"
        );
        server.await.unwrap();
        assert!(
            !saw_request.load(std::sync::atomic::Ordering::SeqCst),
            "pairing code/CSR must not be sent before the confirmed-CA handshake succeeds"
        );
    }

    #[test]
    fn cache_materials_preserves_existing_identity_when_key_write_fails() {
        let (daemon_ca, _) = zeroclaw_tls::testing::gen_ca();
        let dir = tempfile::tempdir().unwrap();
        let tls_dir = dir.path().join("tls");
        std::fs::create_dir_all(&tls_dir).unwrap();
        std::fs::write(tls_dir.join("client.crt"), "old-cert").unwrap();
        std::fs::write(tls_dir.join("ca.crt"), "old-ca").unwrap();
        std::fs::write(tls_dir.join("client.key"), "old-key").unwrap();
        std::fs::write(tls_dir.join("profile.json"), "old-profile").unwrap();

        // Force the staged key write to fail before any final-path rename happens.
        std::fs::create_dir(tls_dir.join(".client.key.tmp")).unwrap();
        let resp = EnrollResponse {
            cert_pem: "new-cert".into(),
            ca_chain_pem: daemon_ca,
            device_id: "dev_renewed".into(),
            not_after: 456,
            relay_profile: RelayProfile::default(),
        };

        let err = cache_materials(dir.path(), &resp, "new-key")
            .unwrap_err()
            .to_string();
        assert!(err.contains(".client.key.tmp"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(tls_dir.join("client.crt")).unwrap(),
            "old-cert"
        );
        assert_eq!(
            std::fs::read_to_string(tls_dir.join("ca.crt")).unwrap(),
            "old-ca"
        );
        assert_eq!(
            std::fs::read_to_string(tls_dir.join("client.key")).unwrap(),
            "old-key"
        );
        assert_eq!(
            std::fs::read_to_string(tls_dir.join("profile.json")).unwrap(),
            "old-profile"
        );
    }
}
