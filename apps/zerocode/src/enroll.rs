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
    let sas = crate::client_crypto::enrollment_sas(code, &ca_fp);
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

/// Generate a CSR and its software private key. A mobile build can replace this
/// call site with a hardware-keystore signer so the key is non-exportable (A5);
/// the desktop path expects an extractable software key it can persist to
/// `client.key`.
fn software_csr(subject_hint: &str) -> Result<(String, zeroize::Zeroizing<String>)> {
    crate::client_crypto::generate_client_csr(subject_hint).context("generating client CSR")
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

/// Wall-clock ceiling on ONE enrollment exchange, covering every step of it:
/// the TCP connect (or the relay dial), the TLS handshake, the request write,
/// and the bounded response read. The response deadline only starts once a TLS
/// session exists, so without this an endpoint that never completes a connect or
/// a handshake - unreachable, or accepting and then silent - holds the client
/// forever.
///
/// 30s: the 15s response window nests inside it with room left for a slow
/// connect and handshake on a poor link, so an exchange that is merely slow is
/// not cut off before its own read deadline can report the real fault.
///
/// Deliberately per-exchange, not one budget over the whole enrollment: the
/// operator's SAS confirmation sits between the trust fetch and the POST, and
/// that prompt is a human with no deadline.
const ENROLL_EXCHANGE_TIMEOUT_SECS: u64 = 30;

/// Run one enrollment exchange under [`ENROLL_EXCHANGE_TIMEOUT_SECS`].
async fn within_exchange_budget<T>(
    what: &str,
    exchange: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(ENROLL_EXCHANGE_TIMEOUT_SECS),
        exchange,
    )
    .await
    {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "{what} did not complete within {ENROLL_EXCHANGE_TIMEOUT_SECS}s; the \
             enrollment endpoint is unreachable, or it accepted the connection and \
             then stopped responding"
        ),
    }
}

/// Fetch the daemon CA over provisional TLS. This preflight sends no pairing code
/// and no CSR; the operator confirms the returned CA via SAS before it is trusted.
async fn fetch_enroll_trust(host: &str, port: u16) -> Result<EnrollTrustResponse> {
    within_exchange_budget("the enrollment trust fetch", async {
        let tcp = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("connecting to enrollment endpoint {host}:{port}"))?;
        fetch_enroll_trust_on_stream(tcp, host, host).await
    })
    .await
}

async fn fetch_enroll_trust_via_relay(
    relay: &crate::client::RelayDial,
) -> Result<EnrollTrustResponse> {
    within_exchange_budget("the enrollment trust fetch through the relay", async {
        // `_pump` is held for exactly as long as the stream is in use. Dropping
        // it retires the tunnel, and it is dropped by every way out of this
        // block - including the budget above expiring, which drops the whole
        // future without running anything written here.
        let (stream, _pump) = crate::client::dial_enrollment_through_relay(relay)
            .await?
            .split();
        fetch_enroll_trust_on_stream(stream, RELAY_ENROLL_SERVER_NAME, RELAY_ENROLL_SERVER_NAME)
            .await
    })
    .await
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
    within_exchange_budget("the enrollment request", async {
        let tcp = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("connecting to enrollment endpoint {host}:{port}"))?;
        post_enroll_on_stream(tcp, host, host, code, csr_pem, trusted_ca_pem).await
    })
    .await
}

async fn post_enroll_via_relay(
    relay: &crate::client::RelayDial,
    code: &str,
    csr_pem: &str,
    trusted_ca_pem: &str,
) -> Result<EnrollResponse> {
    within_exchange_budget("the enrollment request through the relay", async {
        // Held for the life of the exchange; see `fetch_enroll_trust_via_relay`.
        let (stream, _pump) = crate::client::dial_enrollment_through_relay(relay)
            .await?
            .split();
        post_enroll_on_stream(
            stream,
            RELAY_ENROLL_SERVER_NAME,
            RELAY_ENROLL_SERVER_NAME,
            code,
            csr_pem,
            trusted_ca_pem,
        )
        .await
    })
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

    let raw = read_enroll_response(&mut tls).await?;
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

    let raw = read_enroll_response(&mut tls).await?;
    parse_http_json(&raw)
}

/// Largest enrollment response this client will buffer. An enrollment reply is a
/// certificate, a CA chain, and a small relay profile; the daemon bounds its own
/// side of the exchange at the same figure. Mirrors that bound so a hostile
/// endpoint or relay cannot grow the client's buffer without limit.
const MAX_ENROLL_RESPONSE_BYTES: usize = 64 * 1024;

/// Wall-clock ceiling on reading an enrollment response, mirroring the daemon's
/// own connection timeout so a peer cannot hold the stream open indefinitely.
const ENROLL_READ_TIMEOUT_SECS: u64 = 15;

/// Read an enrollment response under both a byte cap and a deadline.
///
/// `read_to_end` alone is unbounded in both dimensions, and these helpers run
/// BEFORE the response is trusted: the CA fetch happens before SAS confirmation,
/// and the enroll POST before any certificate is persisted. A malicious endpoint
/// (or a relay in the path) could stream arbitrary bytes, or simply stall, while
/// the client grew its buffer.
async fn read_enroll_response<S>(tls: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let read = async {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = tls.read(&mut chunk).await?;
            if n == 0 {
                return Ok::<Vec<u8>, std::io::Error>(raw);
            }
            if raw.len() + n > MAX_ENROLL_RESPONSE_BYTES {
                return Err(std::io::Error::other(format!(
                    "enrollment response exceeded {MAX_ENROLL_RESPONSE_BYTES} bytes; refusing to buffer further"
                )));
            }
            raw.extend_from_slice(&chunk[..n]);
        }
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(ENROLL_READ_TIMEOUT_SECS),
        read,
    )
    .await
    {
        Ok(r) => r.context("read enrollment response"),
        Err(_) => anyhow::bail!(
            "enrollment response timed out after {ENROLL_READ_TIMEOUT_SECS}s; \
             the endpoint stopped sending or is stalling the stream"
        ),
    }
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
    Ok(crate::client_crypto::cert_sha256_fingerprint(der.as_ref()))
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

/// Marker written immediately before the first publication rename and removed
/// after the last. Its presence means a publication was interrupted part-way.
/// It carries the manifest of the generation being published, so recovery can
/// tell that generation apart from whatever else is on disk.
const PUBLISH_MARKER: &str = ".publish.pending";

/// First line of the marker. A marker that does not start with this tag was not
/// written by this publication contract, so recovery refuses it rather than
/// guessing which files belong together.
const PUBLISH_MANIFEST_TAG: &str = "zerocode-publish-v1";

/// Durable record of the generation that is CURRENTLY published. It is written
/// after the last rename and before the marker is cleared, so it outlives the
/// marker: a run that finds no marker can still tell a coherent generation from
/// a mixed one. This is the fail-closed net for a marker that never became
/// durable, which is reachable on any platform where the directory entry cannot
/// be fsynced (see `sync_dir_where_supported`).
const PUBLISHED_MANIFEST: &str = "published.manifest";

/// Staged name for the record, so it is installed by the same replace primitive
/// as the credentials and is never observed half-written.
const STAGED_PUBLISHED_MANIFEST: &str = ".published.manifest.tmp";

/// First line of the published-generation record. Distinct from the marker tag,
/// so neither file can ever be parsed as the other.
const PUBLISHED_MANIFEST_TAG: &str = "zerocode-published-v1";

/// Lock file serialising credential publication within one config directory.
const PUBLISH_LOCK: &str = ".publish.lock";

/// How long to wait for another process's publication before refusing. One
/// publication is four small writes and a few fsyncs, so a healthy contender
/// clears in milliseconds; this absorbs a slow disk while keeping a stuck holder
/// an actionable error rather than a hang.
const PUBLISH_LOCK_WAIT_SECS: u64 = 5;

/// Poll interval while waiting for the lock.
const PUBLISH_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Error label for the in-progress marker, used in parse diagnostics.
const MARKER_LABEL: &str = "publication marker";

/// Error label for the published-generation record.
const PUBLISHED_LABEL: &str = "published-credential manifest";

/// Length of a SHA-256 digest in lowercase hex.
const DIGEST_HEX_LEN: usize = 64;

/// One file of a credential generation.
struct Material {
    /// Staged name under `<config_dir>/tls`, written before the marker exists.
    staged: &'static str,
    /// Published name under `<config_dir>/tls`.
    published: &'static str,
    /// The private key is written `0600`; cert, CA, and profile are public.
    private: bool,
}

/// How many files make up one credential generation.
const MATERIAL_COUNT: usize = 4;

/// The staged files and their published names, in publication order.
const STAGED_MATERIALS: [Material; MATERIAL_COUNT] = [
    Material {
        staged: ".client.crt.tmp",
        published: "client.crt",
        private: false,
    },
    Material {
        staged: ".ca.crt.tmp",
        published: "ca.crt",
        private: false,
    },
    Material {
        staged: ".client.key.tmp",
        published: "client.key",
        private: true,
    },
    Material {
        staged: ".profile.json.tmp",
        published: "profile.json",
        private: false,
    },
];

/// A credential generation staged under `<config_dir>/tls`, ready to publish.
struct StagedGeneration {
    tls_dir: std::path::PathBuf,
    /// Marker contents: the tag plus the digest of every staged material.
    manifest: String,
}

/// Stage every material and make the bytes AND their directory entries durable.
/// No marker exists yet, so a crash at any point here leaves the previous
/// generation published and untouched, and the stale `.tmp` files are ignored
/// and overwritten by the next attempt.
fn stage_generation(tls_dir: &Path, payloads: [&[u8]; MATERIAL_COUNT]) -> Result<StagedGeneration> {
    let mut digests = Vec::with_capacity(MATERIAL_COUNT);
    for (material, bytes) in STAGED_MATERIALS.iter().zip(payloads) {
        write_durable(&tls_dir.join(material.staged), bytes, material.private)?;
        digests.push(crate::client_crypto::sha256_hex(bytes));
    }
    sync_dir_where_supported(tls_dir)?;
    Ok(StagedGeneration {
        tls_dir: tls_dir.to_path_buf(),
        manifest: render_manifest(PUBLISH_MANIFEST_TAG, &digests),
    })
}

/// Render a manifest: the tag line, then one `<digest> <published-name>` line
/// per material in `STAGED_MATERIALS` order. The same body serves the in-flight
/// marker and the published-generation record; only the tag differs.
fn render_manifest(tag: &str, digests: &[String]) -> String {
    let mut out = String::from(tag);
    out.push('\n');
    for (material, digest) in STAGED_MATERIALS.iter().zip(digests) {
        out.push_str(digest);
        out.push(' ');
        out.push_str(material.published);
        out.push('\n');
    }
    out
}

/// Make the marker durable, which opens the recovery window. From here the
/// staged set is complete and self-describing, so every state a crash can leave
/// carries the manifest that decides whether the set can still be assembled.
///
/// The manifest is written `0600` because it digests the private key alongside
/// the public materials.
fn mark_generation(staged: &StagedGeneration) -> Result<()> {
    write_durable(
        &staged.tls_dir.join(PUBLISH_MARKER),
        staged.manifest.as_bytes(),
        true,
    )?;
    sync_dir_where_supported(&staged.tls_dir)
}

/// Exclusive inter-process lock over one config directory's credential cache.
///
/// Publication stages under FIXED `.tmp` names and one FIXED marker, so two
/// zerocode processes sharing a config directory interleave without this: one
/// can classify a generation while the other overwrites the staged files under
/// it, and the first then publishes a mixed set or records a manifest that
/// describes neither generation. The crash-phase tests cannot see this, because
/// it is a concurrency fault, not an interruption. The lock covers the whole
/// stage -> mark -> rename -> record sequence, and the recovery/validation path
/// that reads the same files.
///
/// `std::fs::File::try_lock` is the primitive: `flock(LOCK_EX)` on Unix,
/// `LockFileEx` with `LOCKFILE_EXCLUSIVE_LOCK` on Windows. One call, no
/// dependency, and no platform-specific code to review. On Unix the lock is
/// advisory, which binds every process that takes it - every zerocode - and
/// leaves an operator's own edits to the directory unaffected.
#[derive(Debug)]
struct PublishLock {
    file: std::fs::File,
}

impl PublishLock {
    /// Take the lock, waiting up to `wait` for a concurrent publication to
    /// finish. Bounded on purpose: a lock held by a wedged process must surface
    /// as an error naming the file, never as an enrollment that hangs.
    ///
    /// The wait blocks the calling thread. Publication is already blocking file
    /// I/O (every write is fsynced), and enrollment is a one-shot startup step,
    /// so this does not introduce a new kind of stall.
    fn acquire_within(tls_dir: &Path, wait: std::time::Duration) -> Result<Self> {
        let path = tls_dir.join(PUBLISH_LOCK);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        anyhow::bail!(
                            "another process is publishing credentials and still holds {} \
                             after {}s. Let the other zerocode finish enrolling or renewing, \
                             then try again.",
                            path.display(),
                            wait.as_secs()
                        );
                    }
                    std::thread::sleep(PUBLISH_LOCK_POLL);
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(anyhow::Error::new(e))
                        .with_context(|| format!("locking {}", path.display()));
                }
            }
        }
    }

    fn acquire(tls_dir: &Path) -> Result<Self> {
        Self::acquire_within(
            tls_dir,
            std::time::Duration::from_secs(PUBLISH_LOCK_WAIT_SECS),
        )
    }
}

impl Drop for PublishLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock on both platforms; unlocking
        // first makes the release explicit rather than a side effect of drop
        // order. The lock file itself is left in place: it is the lock's
        // identity, and removing it would let a later process take a lock on a
        // different inode while this one is still held.
        let _ = self.file.unlock();
    }
}

/// Lock a credential cache that already exists. `None` when the directory is
/// absent: nothing was ever published there, so there is nothing to serialise
/// and no reason for a read path to create the directory.
fn lock_existing_cache(tls_dir: &Path) -> Result<Option<PublishLock>> {
    if !tls_dir.exists() {
        return Ok(None);
    }
    PublishLock::acquire(tls_dir).map(Some)
}

/// Replace `published` with `staged` in one step, on every platform.
///
/// `std::fs::rename` IS the atomic-replace primitive here; its documented
/// cross-platform contract is "renames a file or directory to a new name,
/// replacing the original file if `to` already exists". On Unix that is POSIX
/// `rename(2)`. On Windows the standard library issues `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING`, falling back to `SetFileInformationByHandle`
/// with `FileRenameInfoEx` / `FILE_RENAME_FLAG_REPLACE_IF_EXISTS` when the
/// destination's read-only attribute denies the first call. A renewal over an
/// existing generation therefore replaces the destination rather than failing,
/// which is what makes `cache_materials` re-runnable.
///
/// Deliberately NOT remove-then-rename: unlinking the destination first would
/// open a window where the credential is absent entirely, which is the mixed or
/// missing generation the marker and the published manifest exist to prevent.
///
/// Residual Windows risk: another process holding the destination open without
/// delete-sharing (a scanner, a second zerocode) can still make the call fail.
/// That failure is fail-safe, not fail-open - the error propagates with the
/// marker still on disk and the previous generation intact, so the next run
/// recovers.
fn replace_file(staged: &Path, published: &Path) -> Result<()> {
    std::fs::rename(staged, published)
        .with_context(|| format!("installing {}", published.display()))
}

/// Rename one staged material over its published name.
fn install_material(tls_dir: &Path, material: &Material) -> Result<()> {
    replace_file(
        &tls_dir.join(material.staged),
        &tls_dir.join(material.published),
    )
}

/// Complete a publication interrupted between the first and last rename, or
/// refuse when the recorded generation can no longer be assembled from disk.
///
/// The credential set is four files published with four renames, which is not
/// one atomic step. A crash in between leaves a mixed set - most damagingly a
/// NEW `client.crt` beside the OLD `client.key`, which authenticates as
/// neither. Making the swap genuinely atomic needs a directory-swap or symlink
/// layout that changes the published `<config_dir>/tls/` contract and does not
/// port cleanly to Windows, so the transition is made durable and verifiable
/// instead:
///
/// - Every staged `.tmp` file is fsynced, and its directory entry fsynced,
///   BEFORE the marker exists. A crash during staging therefore leaves no
///   marker, and the stale `.tmp` files are ignored.
/// - The marker is a manifest: it records the digest of every staged material
///   and is itself fsynced, directory entry included, before the first rename.
///   Any state reachable after that carries the manifest.
/// - Recovery matches every material against the manifest BEFORE it moves
///   anything. A material must be either staged with the recorded digest or
///   already published with it. Anything else - a lost staged file, a stale
///   published file, an unreadable manifest - is a hard error that leaves the
///   marker in place, because the generation cannot be reconstructed and the
///   partial set must not be trusted.
/// - The published-generation record is written AFTER the last rename and
///   BEFORE the marker is cleared, so every state a crash can leave is covered
///   by one of the two: a surviving marker means "recover me", and a marker that
///   never became durable leaves a record that no longer matches the files on
///   disk. `validate_published_generation` turns that second case into a
///   refusal instead of a silently trusted mixed set.
/// - The marker is removed only once every rename AND the record are durable,
///   so the window closes exactly when the set is consistent and self-describing.
///
/// This is also the commit path for a fresh publication (see `cache_materials`),
/// so the published set is only ever assembled by the code a crashed run uses.
///
/// Callers run this before READING the materials and before staging new ones,
/// so an interrupted publication is repaired rather than observed.
/// The recovery itself. The caller holds the publication lock, so the staged
/// files and the marker cannot move under it. Reached from `cache_materials`
/// before it stages, and from `recover_and_validate` at startup.
fn finish_pending_publish_locked(tls_dir: &Path) -> Result<()> {
    let tls_dir = tls_dir.to_path_buf();
    let marker = tls_dir.join(PUBLISH_MARKER);
    let raw = match std::fs::read_to_string(&marker) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("reading {}", marker.display()));
        }
    };
    let digests = parse_manifest(&raw, PUBLISH_MANIFEST_TAG, MARKER_LABEL)
        .with_context(|| format!("reading the credential manifest in {}", marker.display()))?;

    // Classify the whole set BEFORE moving anything: a rename applied on the way
    // to a refusal would publish exactly the mixed generation this recovery
    // exists to prevent.
    let mut pending = Vec::new();
    let mut unrecoverable = Vec::new();
    for (material, digest) in STAGED_MATERIALS.iter().zip(&digests) {
        let staged = tls_dir.join(material.staged);
        let published = tls_dir.join(material.published);
        if file_digest(&staged)?.as_deref() == Some(digest.as_str()) {
            pending.push(material);
        } else if file_digest(&published)?.as_deref() != Some(digest.as_str()) {
            unrecoverable.push(format!(
                "{} (staged as {})",
                published.display(),
                staged.display()
            ));
        }
    }
    if !unrecoverable.is_empty() {
        anyhow::bail!(
            "an interrupted credential publication cannot be completed: {} \
             no longer match the generation recorded in {}. The marker is kept \
             so the partial set is never trusted; remove the credentials in {} \
             and enroll again.",
            unrecoverable.join(", "),
            marker.display(),
            tls_dir.display()
        );
    }

    for material in pending {
        install_material(&tls_dir, material).with_context(|| {
            format!(
                "completing interrupted publish of {}",
                tls_dir.join(material.published).display()
            )
        })?;
    }
    // Every rename must be durable before the marker stops guarding the set.
    sync_dir_where_supported(&tls_dir)?;
    // The record of what is published NOW must exist before the marker stops
    // guarding it: from the moment the marker is gone, this record is the only
    // evidence that the four files belong to one generation.
    record_published_generation(&tls_dir, &digests)?;
    std::fs::remove_file(&marker).with_context(|| format!("clearing {}", marker.display()))?;
    sync_dir_where_supported(&tls_dir)
}

/// Verify the published credential set against the record written by the last
/// completed publication, and refuse to run on a set that does not match it.
///
/// `finish_pending_publish` handles the states a marker survives. This handles
/// the state it does NOT: a crash whose marker removal became durable while a
/// rename did not, which leaves a mixed generation and no marker to point at it.
/// A directory entry cannot be fsynced portably, so that reordering is reachable
/// on Windows in particular, and this check is what keeps it fail-closed instead
/// of silently trusting a NEW certificate beside an OLD key.
///
/// Order matters: callers run this AFTER `finish_pending_publish`, never before.
/// During the marker window the record still describes the PREVIOUS generation
/// by design, and recovery refreshes it; validating first would refuse a state
/// that is perfectly recoverable.
///
/// A missing record is the pre-record (legacy) cache or a machine that has never
/// enrolled. A complete set is adopted and recorded so later runs are covered;
/// an incomplete one is left alone, because without a record there is no
/// evidence of a mixed generation and refusing would break caches that were
/// always partial.
///
/// The residual window this leaves: a crash that loses BOTH the marker removal
/// and the record write while a rename is missing looks exactly like a legacy
/// cache, and is adopted. Closing it would mean refusing every cache written
/// before this record existed, which breaks working installs to guard a
/// double-fault - the record write is ordered before the marker removal, so a
/// filesystem that journals metadata in order never reaches it.
/// Recover an interrupted publication and then validate the published
/// generation, under ONE lock.
///
/// This is what startup calls. Taking the lock once matters: with two separate
/// acquisitions another process could complete a publication in the gap, and
/// this run would then compare fresh credentials against the record it read a
/// moment earlier and refuse a cache that is in fact coherent.
pub fn recover_and_validate(config_dir: &Path) -> Result<()> {
    let tls_dir = config_dir.join("tls");
    let Some(_lock) = lock_existing_cache(&tls_dir)? else {
        return Ok(());
    };
    finish_pending_publish_locked(&tls_dir)
        .context("completing an interrupted credential publication")?;
    validate_published_generation_locked(&tls_dir)
        .context("validating the published credential set")
}

/// The validation itself. The caller holds the publication lock.
fn validate_published_generation_locked(tls_dir: &Path) -> Result<()> {
    let tls_dir = tls_dir.to_path_buf();
    let record = tls_dir.join(PUBLISHED_MANIFEST);
    let raw = match std::fs::read_to_string(&record) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return adopt_published_generation(&tls_dir);
        }
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .with_context(|| format!("reading {}", record.display()));
        }
    };
    let digests = parse_manifest(&raw, PUBLISHED_MANIFEST_TAG, PUBLISHED_LABEL)
        .with_context(|| format!("reading the credential manifest in {}", record.display()))?;

    let mut mismatched = Vec::new();
    for (material, digest) in STAGED_MATERIALS.iter().zip(&digests) {
        let published = tls_dir.join(material.published);
        if file_digest(&published)?.as_deref() != Some(digest.as_str()) {
            mismatched.push(published.display().to_string());
        }
    }
    if !mismatched.is_empty() {
        anyhow::bail!(
            "the published credential set is inconsistent: {} no longer match the \
             generation recorded in {}. A publication was interrupted without \
             leaving its marker, so the set may pair a new certificate with an \
             old key and would authenticate as neither; remove the credentials \
             in {} and enroll again.",
            mismatched.join(", "),
            record.display(),
            tls_dir.display()
        );
    }
    Ok(())
}

/// Record the set already on disk when no record exists yet, so an upgrade from
/// a pre-record cache is protected from the next crash without re-enrolling.
///
/// Best-effort: a set that cannot be recorded (a read-only credential directory)
/// still runs, because a missing record is not evidence that anything is wrong.
/// The run is told, so the missing protection is not silent.
fn adopt_published_generation(tls_dir: &Path) -> Result<()> {
    let mut digests = Vec::with_capacity(MATERIAL_COUNT);
    for material in &STAGED_MATERIALS {
        match file_digest(&tls_dir.join(material.published))? {
            Some(digest) => digests.push(digest),
            // Not a complete generation: nothing to adopt, nothing to refuse.
            None => return Ok(()),
        }
    }
    if let Err(e) = record_published_generation(tls_dir, &digests) {
        eprintln!(
            "zerocode: could not record the published credential generation ({e:#}); \
             an interrupted publication may go undetected until the next enrollment."
        );
    }
    Ok(())
}

/// Write the record of the generation that is published now. Staged and renamed
/// through the same replace primitive as the credentials, so a crash leaves the
/// previous record rather than a truncated one. Written `0600` like the marker:
/// it digests the private key alongside the public materials.
fn record_published_generation(tls_dir: &Path, digests: &[String]) -> Result<()> {
    let body = render_manifest(PUBLISHED_MANIFEST_TAG, digests);
    let staged = tls_dir.join(STAGED_PUBLISHED_MANIFEST);
    write_durable(&staged, body.as_bytes(), true)?;
    replace_file(&staged, &tls_dir.join(PUBLISHED_MANIFEST))?;
    sync_dir_where_supported(tls_dir)
}

/// Parse a manifest into the recorded digest of each material, in
/// `STAGED_MATERIALS` order. The published names are checked so a manifest
/// describing a different set is refused rather than matched positionally.
/// `label` names the file in diagnostics, since the marker and the
/// published-generation record share this body format.
fn parse_manifest(raw: &str, expected_tag: &str, label: &str) -> Result<Vec<String>> {
    let mut lines = raw.lines();
    let tag = lines.next().unwrap_or_default().trim();
    if tag != expected_tag {
        anyhow::bail!("unrecognised {label} format {tag:?}");
    }
    let mut digests = Vec::with_capacity(MATERIAL_COUNT);
    for material in &STAGED_MATERIALS {
        let line = lines.next().with_context(|| format!("truncated {label}"))?;
        let (digest, name) = line
            .trim()
            .split_once(' ')
            .with_context(|| format!("malformed {label} entry {line:?}"))?;
        if name != material.published {
            anyhow::bail!(
                "{label} lists {name:?} where {} is expected",
                material.published
            );
        }
        if digest.len() != DIGEST_HEX_LEN || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            anyhow::bail!(
                "{label} holds a malformed digest for {}",
                material.published
            );
        }
        digests.push(digest.to_ascii_lowercase());
    }
    Ok(digests)
}

/// SHA-256 of a file's bytes, or `None` when the file is absent.
fn file_digest(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(crate::client_crypto::sha256_hex(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e)).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write the cert, key, CA, and cached profile under `<config_dir>/tls`. The key
/// is written `0600` on Unix; the cert/CA/profile are public.
///
/// The four files are ONE generation: they are staged and made durable, then the
/// manifest marker is made durable, and only then are they renamed into place -
/// by the same recovery path a crashed run takes, which also records the
/// published generation before it clears the marker. See `finish_pending_publish`.
///
/// This path REPAIRS rather than refuses: it does not run
/// `validate_published_generation`, so a set that startup refused can still be
/// replaced by enrolling again, which is what that refusal tells the operator
/// to do.
fn cache_materials(config_dir: &Path, resp: &EnrollResponse, key_pem: &str) -> Result<()> {
    // Validate before writing anything. The SAS is bound to this single trust
    // anchor, so refuse to persist a broader
    // root set that the later WSS client would trust but the operator never saw.
    let _ = single_ca_cert_der(&resp.ca_chain_pem)?;

    let tls_dir = config_dir.join("tls");
    std::fs::create_dir_all(&tls_dir).with_context(|| format!("creating {}", tls_dir.display()))?;
    // The directory entry must be durable before it holds credentials.
    sync_dir_where_supported(config_dir)?;
    // Held across the whole sequence below: another zerocode sharing this config
    // directory stages under the same fixed names, and an interleaving between
    // the classification and the four renames publishes a mixed set.
    let _lock = PublishLock::acquire(&tls_dir)?;
    // Repair an interrupted earlier publication before staging over it.
    finish_pending_publish_locked(&tls_dir)?;
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

    let staged = stage_generation(
        &tls_dir,
        [
            resp.cert_pem.as_bytes(),
            resp.ca_chain_pem.as_bytes(),
            key_pem.as_bytes(),
            json.as_bytes(),
        ],
    )?;
    mark_generation(&staged)?;
    finish_pending_publish_locked(&tls_dir)
}

/// Write `bytes` to `path` and fsync them, so the content survives power loss
/// before anything claims it. `private` writes `0600` on Unix, with no
/// world-readable window.
fn write_durable(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if private {
            options.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    {
        // No mode bits here; the tls directory ACL is the guard.
        let _ = private;
    }
    let mut f = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

/// fsync a directory so the entries created, renamed, or removed inside it are
/// durable, not just the file contents. Unix opens the directory and syncs the
/// handle. No other platform offers a portable equivalent, so this is a
/// documented no-op there and the publication relies on the marker alone.
fn sync_dir_where_supported(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let handle = std::fs::File::open(dir)
            .with_context(|| format!("opening {} to sync it", dir.display()))?;
        handle
            .sync_all()
            .with_context(|| format!("syncing {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Leave `<config_dir>/tls` in the state a crash leaves after `renames` of the
/// four publication renames: the new generation staged and marked, the first
/// `renames` materials published, the marker still present. Test-only seam so a
/// regression can build a real interrupted state rather than an approximation.
#[cfg(test)]
pub(crate) fn interrupt_publication_for_test(
    config_dir: &Path,
    payloads: [&[u8]; MATERIAL_COUNT],
    renames: usize,
) -> Result<()> {
    let tls_dir = config_dir.join("tls");
    std::fs::create_dir_all(&tls_dir).with_context(|| format!("creating {}", tls_dir.display()))?;
    let staged = stage_generation(&tls_dir, payloads)?;
    mark_generation(&staged)?;
    for material in STAGED_MATERIALS.iter().take(renames) {
        install_material(&tls_dir, material)?;
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
    // Fault-phase coverage for the four-rename credential publication (August
    // human review). A test cannot cut the power, so each phase runs the REAL
    // publication steps and stops at the boundary where the process would have
    // died; the on-disk state is therefore the state a crash at that point
    // actually leaves, not a hand-built approximation of it.
    //
    // The contract every phase asserts: recovery either completes the whole
    // generation or refuses and keeps its marker. It never publishes a mixed
    // set (a NEW cert beside an OLD key), and it never clears the marker while
    // the recorded generation is incomplete.

    /// Recovery from a config dir, taking the lock exactly as the production
    /// entries do. `recover_and_validate` bundles recovery with validation;
    /// these two seams let a test assert one step at a time.
    fn finish_pending_publish(config_dir: &std::path::Path) -> Result<()> {
        let tls_dir = config_dir.join("tls");
        let Some(_lock) = lock_existing_cache(&tls_dir)? else {
            return Ok(());
        };
        finish_pending_publish_locked(&tls_dir)
    }

    /// Validation from a config dir, taking the lock as production does.
    fn validate_published_generation(config_dir: &std::path::Path) -> Result<()> {
        let tls_dir = config_dir.join("tls");
        let Some(_lock) = lock_existing_cache(&tls_dir)? else {
            return Ok(());
        };
        validate_published_generation_locked(&tls_dir)
    }

    /// The generation already published before the publication under test.
    const OLD: [&[u8]; MATERIAL_COUNT] = [b"OLD-CERT", b"OLD-CA", b"OLD-KEY", b"OLD-PROFILE"];
    /// The generation being published.
    const NEW: [&[u8]; MATERIAL_COUNT] = [b"NEW-CERT", b"NEW-CA", b"NEW-KEY", b"NEW-PROFILE"];

    fn publish_old_generation(tls: &std::path::Path) {
        std::fs::create_dir_all(tls).unwrap();
        for (material, bytes) in STAGED_MATERIALS.iter().zip(OLD) {
            std::fs::write(tls.join(material.published), bytes).unwrap();
        }
    }

    /// Every published material belongs to the same generation.
    fn assert_published_generation(tls: &std::path::Path, expected: [&[u8]; MATERIAL_COUNT]) {
        for (material, bytes) in STAGED_MATERIALS.iter().zip(expected) {
            let path = tls.join(material.published);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{} is not the expected generation",
                path.display()
            );
        }
    }

    /// Phase: staged and durable, marker not yet written. The set is not
    /// known-good, so recovery leaves the published generation alone.
    #[test]
    fn interruption_after_staging_leaves_the_published_set_alone() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        stage_generation(&tls, NEW).unwrap();

        finish_pending_publish(dir.path())
            .expect("an unmarked staged set is not a pending publish");

        assert_published_generation(&tls, OLD);
        assert!(!tls.join(PUBLISH_MARKER).exists());
    }

    /// Phases: marker durable with zero renames applied, through every rename
    /// count up to all four with the marker still present. Each is recoverable
    /// into the complete new generation.
    #[test]
    fn every_rename_interruption_completes_the_generation() {
        for renames in 0..=MATERIAL_COUNT {
            let dir = tempfile::tempdir().unwrap();
            let tls = dir.path().join("tls");
            publish_old_generation(&tls);
            interrupt_publication_for_test(dir.path(), NEW, renames).unwrap();
            assert!(
                tls.join(PUBLISH_MARKER).exists(),
                "the marker guards the window after {renames} rename(s)"
            );

            finish_pending_publish(dir.path())
                .unwrap_or_else(|e| panic!("recovery after {renames} rename(s) failed: {e:#}"));

            assert_published_generation(&tls, NEW);
            assert!(
                !tls.join(PUBLISH_MARKER).exists(),
                "the marker must be cleared once the set is consistent"
            );
        }
    }

    /// A first enrollment has no previous generation to fall back to, so an
    /// interruption before any rename must still recover the complete set.
    #[test]
    fn a_first_publication_interrupted_before_any_rename_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        interrupt_publication_for_test(dir.path(), NEW, 0).unwrap();

        finish_pending_publish(dir.path()).expect("recovery must succeed");

        assert_published_generation(&tls, NEW);
        assert!(!tls.join(PUBLISH_MARKER).exists());
    }

    /// A staged material that is not on disk any more means the recorded
    /// generation cannot be assembled. Finishing the other renames is exactly
    /// what leaves a NEW cert beside an OLD key, so recovery refuses, moves
    /// nothing, and keeps the marker.
    #[test]
    fn a_lost_staged_material_is_refused_and_keeps_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        interrupt_publication_for_test(dir.path(), NEW, 1).unwrap();
        std::fs::remove_file(tls.join(".client.key.tmp")).unwrap();

        let err = format!("{:#}", finish_pending_publish(dir.path()).unwrap_err());
        assert!(err.contains("client.key"), "got: {err}");
        assert!(
            tls.join(PUBLISH_MARKER).exists(),
            "an incomplete generation must never clear its marker"
        );
        assert_eq!(std::fs::read(tls.join("client.key")).unwrap(), b"OLD-KEY");
        assert_eq!(
            std::fs::read(tls.join("ca.crt")).unwrap(),
            b"OLD-CA",
            "no rename may be applied on the way to a refusal"
        );
        assert!(tls.join(".ca.crt.tmp").exists());
        // The refusal is stable: a later run must not decide differently.
        assert!(finish_pending_publish(dir.path()).is_err());
    }

    /// A staged material whose bytes do not match the manifest is not part of
    /// the recorded generation, so the set is refused rather than published.
    #[test]
    fn a_staged_material_that_does_not_match_the_manifest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        interrupt_publication_for_test(dir.path(), NEW, 0).unwrap();
        std::fs::write(tls.join(".profile.json.tmp"), b"TORN").unwrap();

        let err = format!("{:#}", finish_pending_publish(dir.path()).unwrap_err());
        assert!(err.contains("profile.json"), "got: {err}");
        assert!(tls.join(PUBLISH_MARKER).exists());
        assert_published_generation(&tls, OLD);
    }

    /// A marker without a parseable manifest describes no generation at all, so
    /// recovery refuses instead of renaming whatever happens to be staged.
    #[test]
    fn a_marker_without_a_manifest_is_refused_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        stage_generation(&tls, NEW).unwrap();
        std::fs::write(tls.join(PUBLISH_MARKER), b"publishing\n").unwrap();

        let err = format!("{:#}", finish_pending_publish(dir.path()).unwrap_err());
        assert!(err.contains("publication marker"), "got: {err}");
        assert!(tls.join(PUBLISH_MARKER).exists());
        assert_published_generation(&tls, OLD);
    }

    /// Staged files WITHOUT a marker are the residue of a crash during staging:
    /// that set is not known-good, so recovery must ignore it rather than
    /// publish a half-written credential.
    #[test]
    fn staged_files_without_a_marker_are_not_published() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        std::fs::create_dir_all(&tls).unwrap();
        std::fs::write(tls.join("client.crt"), b"OLD-CERT").unwrap();
        std::fs::write(tls.join(".client.crt.tmp"), b"HALF-WRITTEN").unwrap();

        finish_pending_publish(dir.path()).expect("no marker is not an error");

        assert_eq!(
            std::fs::read(tls.join("client.crt")).unwrap(),
            b"OLD-CERT",
            "an unmarked staged file must never be published"
        );
    }

    /// A completed publication leaves no marker, so recovery is a no-op and the
    /// published set is untouched.
    #[test]
    fn cache_materials_leaves_no_marker_behind() {
        let dir = tempfile::tempdir().unwrap();
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
        let resp = EnrollResponse {
            cert_pem: "new-cert".into(),
            ca_chain_pem: daemon_ca,
            device_id: "dev_test".into(),
            not_after: 0,
            relay_profile: RelayProfile::default(),
        };
        cache_materials(dir.path(), &resp, "new-key").expect("publish must succeed");
        assert!(
            !dir.path().join("tls").join(PUBLISH_MARKER).exists(),
            "a completed publish must clear its marker"
        );
        let key = dir.path().join("tls").join("client.key");
        assert!(key.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the published key must stay 0600");
        }
    }

    /// The digests the published-generation record currently claims.
    fn recorded_digests(tls: &std::path::Path) -> Vec<String> {
        let raw = std::fs::read_to_string(tls.join(PUBLISHED_MANIFEST))
            .expect("a completed publication must leave a record");
        parse_manifest(&raw, PUBLISHED_MANIFEST_TAG, PUBLISHED_LABEL)
            .expect("the record this code wrote must parse")
    }

    /// The record must describe the bytes that are actually published.
    fn assert_record_matches_disk(tls: &std::path::Path) {
        for (material, digest) in STAGED_MATERIALS.iter().zip(recorded_digests(tls)) {
            let path = tls.join(material.published);
            assert_eq!(
                file_digest(&path).unwrap().as_deref(),
                Some(digest.as_str()),
                "{} does not match the recorded generation",
                path.display()
            );
        }
    }

    fn enroll_response(cert: &str, ca: String, device: &str) -> EnrollResponse {
        EnrollResponse {
            cert_pem: cert.into(),
            ca_chain_pem: ca,
            device_id: device.into(),
            not_after: 0,
            relay_profile: RelayProfile::default(),
        }
    }

    /// The publication primitive must REPLACE an existing destination rather
    /// than fail on it: every rename after the first enrollment has a
    /// destination that already exists. On Unix this is POSIX rename; on Windows
    /// the standard library maps it to `MoveFileExW` with
    /// `MOVEFILE_REPLACE_EXISTING`. The assertion is the same on both, so this
    /// is the regression a Windows run would catch.
    #[test]
    fn replacing_an_existing_destination_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join(STAGED_MATERIALS[0].staged);
        let published = dir.path().join(STAGED_MATERIALS[0].published);
        std::fs::write(&published, b"OLD-CERT").unwrap();
        std::fs::write(&staged, b"NEW-CERT").unwrap();

        replace_file(&staged, &published).expect("replacing a published credential must succeed");

        assert_eq!(std::fs::read(&published).unwrap(), b"NEW-CERT");
        assert!(
            !staged.exists(),
            "the staged file must not survive its own publication"
        );
    }

    /// A renewal republishes over four destinations that all already exist. The
    /// whole set must turn over and the record must follow it, or the next run
    /// refuses a set that is in fact fine.
    #[test]
    fn a_renewal_replaces_every_published_file_and_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();

        cache_materials(
            dir.path(),
            &enroll_response("cert-gen-1", daemon_ca.clone(), "dev_1"),
            "key-gen-1",
        )
        .expect("the first enrollment must publish");
        validate_published_generation(dir.path()).expect("a fresh publication must validate");
        let first = recorded_digests(&tls);

        cache_materials(
            dir.path(),
            &enroll_response("cert-gen-2", daemon_ca, "dev_2"),
            "key-gen-2",
        )
        .expect("a renewal over an existing generation must publish");

        assert_eq!(
            std::fs::read(tls.join("client.crt")).unwrap(),
            b"cert-gen-2"
        );
        assert_eq!(std::fs::read(tls.join("client.key")).unwrap(), b"key-gen-2");
        assert_eq!(
            cached_profile(dir.path()).unwrap().device_id,
            "dev_2",
            "the renewed profile must replace the previous one"
        );
        assert!(!tls.join(PUBLISH_MARKER).exists());
        assert_ne!(
            recorded_digests(&tls),
            first,
            "the record must follow the generation it describes"
        );
        assert_record_matches_disk(&tls);
        validate_published_generation(dir.path()).expect("the renewed generation must validate");
    }

    /// The state the marker cannot cover: one rename applied, and the marker
    /// gone before the rest were. Nothing on disk says a publication was in
    /// flight, so only the record can tell that the set is mixed.
    #[test]
    fn a_mixed_set_whose_marker_did_not_survive_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        validate_published_generation(dir.path()).expect("the published set is recorded");

        interrupt_publication_for_test(dir.path(), NEW, 1).unwrap();
        std::fs::remove_file(tls.join(PUBLISH_MARKER)).unwrap();

        finish_pending_publish(dir.path()).expect("without a marker there is nothing to recover");
        let err = format!(
            "{:#}",
            validate_published_generation(dir.path())
                .expect_err("a mixed set must never be accepted")
        );
        assert!(err.contains("client.crt"), "got: {err}");
        assert!(
            err.contains("enroll again"),
            "the refusal must say how to recover, got: {err}"
        );
        assert!(
            !err.contains("client.key"),
            "only the file that moved is inconsistent, got: {err}"
        );
    }

    /// A cache published before this record existed must keep working: the set
    /// is adopted and recorded, not refused. The adopted record then guards it.
    #[test]
    fn a_credential_set_without_a_record_is_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        assert!(!tls.join(PUBLISHED_MANIFEST).exists());

        validate_published_generation(dir.path()).expect("an upgrade must not break a valid cache");

        assert_published_generation(&tls, OLD);
        assert_record_matches_disk(&tls);
        assert_eq!(
            recorded_digests(&tls)[0],
            crate::client_crypto::sha256_hex(OLD[0])
        );
        validate_published_generation(dir.path()).expect("the adopted record must validate");

        std::fs::write(tls.join("client.key"), b"MIXED-KEY").unwrap();
        assert!(
            validate_published_generation(dir.path()).is_err(),
            "the adopted record must guard the set from then on"
        );
    }

    /// Nothing to validate is not a failure: a machine that never enrolled, or
    /// one whose cache was always partial, must still start.
    #[test]
    fn an_absent_or_partial_credential_set_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        validate_published_generation(dir.path()).expect("a machine with no credentials must run");
        assert!(!tls.join(PUBLISHED_MANIFEST).exists());

        std::fs::create_dir_all(&tls).unwrap();
        std::fs::write(tls.join("client.crt"), OLD[0]).unwrap();
        validate_published_generation(dir.path()).expect("a partial cache must not be refused");
        assert!(
            !tls.join(PUBLISHED_MANIFEST).exists(),
            "an incomplete set must not be recorded as a generation"
        );
    }

    /// The window between the last rename and the record: the marker is still
    /// there, so the record still describes the PREVIOUS generation. Validation
    /// alone must not accept that, and the startup order must repair it.
    #[test]
    fn a_crash_before_the_record_is_repaired_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        validate_published_generation(dir.path()).expect("the old set is recorded");

        interrupt_publication_for_test(dir.path(), NEW, MATERIAL_COUNT).unwrap();
        assert_published_generation(&tls, NEW);

        assert!(
            validate_published_generation(dir.path()).is_err(),
            "a record that describes other bytes must never be accepted"
        );

        finish_pending_publish(dir.path()).expect("the marker still guards this state");

        assert!(!tls.join(PUBLISH_MARKER).exists());
        assert_published_generation(&tls, NEW);
        assert_record_matches_disk(&tls);
        validate_published_generation(dir.path()).expect("the refreshed record must validate");
    }

    /// Take the publication lock the way a SECOND process would: a separate file
    /// handle on the same lock file. On both platforms the lock is per-handle, so
    /// this conflicts with the crate's own lock exactly as another process does.
    fn foreign_lock(tls: &std::path::Path) -> std::fs::File {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(tls.join(PUBLISH_LOCK))
            .unwrap();
        file.try_lock()
            .expect("the lock must be free at this point");
        file
    }

    /// A concurrent publication must not be able to interleave with this one.
    /// While another holder has the lock, `cache_materials` must not have staged
    /// anything; once the holder releases, it must complete a whole generation.
    #[test]
    fn a_concurrent_publication_cannot_interleave_with_this_one() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        std::fs::create_dir_all(&tls).unwrap();
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();

        let held = foreign_lock(&tls);
        let staged_during_hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer = staged_during_hold.clone();
        let watch_dir = tls.clone();
        let holder = std::thread::spawn(move || {
            // Hold long enough that a publication ignoring the lock would have
            // staged and renamed well within the window.
            for _ in 0..8 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                if STAGED_MATERIALS
                    .iter()
                    .any(|m| watch_dir.join(m.staged).exists())
                {
                    observer.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            drop(held);
        });

        let started = std::time::Instant::now();
        cache_materials(
            dir.path(),
            &enroll_response("cert-gen-1", daemon_ca, "dev_1"),
            "key-gen-1",
        )
        .expect("publication must proceed once the lock is free");
        let waited = started.elapsed();
        holder.join().unwrap();

        assert!(
            !staged_during_hold.load(std::sync::atomic::Ordering::SeqCst),
            "no credential may be staged while another process holds the lock"
        );
        assert!(
            waited >= std::time::Duration::from_millis(150),
            "publication must wait for the lock, not race it (waited {waited:?})"
        );
        assert_eq!(
            std::fs::read(tls.join("client.crt")).unwrap(),
            b"cert-gen-1"
        );
        assert_record_matches_disk(&tls);
        validate_published_generation(dir.path()).expect("the published set must be coherent");
    }

    /// A holder that never lets go must produce an actionable error naming the
    /// lock file, not a hang.
    #[test]
    fn a_lock_that_is_never_released_fails_with_an_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        std::fs::create_dir_all(&tls).unwrap();
        let _held = foreign_lock(&tls);

        let started = std::time::Instant::now();
        let err = format!(
            "{:#}",
            PublishLock::acquire_within(&tls, std::time::Duration::from_millis(120))
                .expect_err("a lock held by another process must not block forever")
        );

        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the wait must be bounded"
        );
        assert!(err.contains(PUBLISH_LOCK), "got: {err}");
        assert!(err.contains("another process"), "got: {err}");
    }

    /// The lock must be released when publication ends, or the next enrollment
    /// on this machine deadlocks against a lock nobody holds.
    #[test]
    fn the_lock_is_released_after_publication_and_after_validation() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();

        cache_materials(
            dir.path(),
            &enroll_response("cert-gen-1", daemon_ca.clone(), "dev_1"),
            "key-gen-1",
        )
        .expect("first publication must succeed");
        drop(foreign_lock(&tls));

        recover_and_validate(dir.path()).expect("startup must run against a published set");
        drop(foreign_lock(&tls));

        cache_materials(
            dir.path(),
            &enroll_response("cert-gen-2", daemon_ca, "dev_2"),
            "key-gen-2",
        )
        .expect("a renewal must be able to take the lock again");
        assert_eq!(
            std::fs::read(tls.join("client.crt")).unwrap(),
            b"cert-gen-2"
        );
    }

    /// Startup takes the same lock as publication. Reading a cache while another
    /// process is mid-publication is how a coherent set gets reported as
    /// inconsistent: the record is refreshed between the renames and the marker
    /// removal, so an unlocked read can land on a generation the record does not
    /// describe yet and refuse a machine that is fine.
    #[test]
    fn startup_waits_for_a_publication_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
        cache_materials(
            dir.path(),
            &enroll_response("cert-gen-1", daemon_ca, "dev_1"),
            "key-gen-1",
        )
        .expect("a published set to read");

        let held = foreign_lock(&tls);
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(held);
        });

        let started = std::time::Instant::now();
        recover_and_validate(dir.path()).expect("startup must succeed once the lock is free");
        let waited = started.elapsed();
        holder.join().unwrap();

        assert!(
            waited >= std::time::Duration::from_millis(150),
            "startup must wait for a publication in flight rather than read across it \
             (waited {waited:?})"
        );
    }

    /// A machine that has never enrolled has no credential directory. Startup
    /// must not create one just to take a lock over nothing.
    #[test]
    fn startup_does_not_create_a_credential_directory_to_lock_it() {
        let dir = tempfile::tempdir().unwrap();
        recover_and_validate(dir.path()).expect("a machine with no credentials must start");
        assert!(!dir.path().join("tls").exists());
    }

    /// The marker and the record share a body format and a directory. Their tags
    /// keep them apart, so neither can be read as the other.
    #[test]
    fn a_publication_marker_is_not_accepted_as_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls);
        let staged = stage_generation(&tls, NEW).unwrap();
        std::fs::write(tls.join(PUBLISHED_MANIFEST), staged.manifest.as_bytes()).unwrap();

        let err = format!(
            "{:#}",
            validate_published_generation(dir.path())
                .expect_err("a marker must not pass as the published record")
        );
        assert!(err.contains(PUBLISHED_LABEL), "got: {err}");
    }

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
    fn sas_is_stable_for_daemon_comparison() {
        // The client SAS computation must stay stable for daemon comparison.
        let fp = "aabbccdd";
        assert_eq!(
            crate::client_crypto::enrollment_sas("270391", fp),
            crate::client_crypto::enrollment_sas("270391", fp)
        );
    }

    #[test]
    fn enrollment_rejects_appended_ca_before_cache() {
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
        let (rogue_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
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
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
        let (rogue_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
        let err = ensure_response_ca_matches_confirmed(&daemon_ca, &rogue_ca)
            .unwrap_err()
            .to_string();
        assert!(err.contains("operator-confirmed daemon CA"), "got: {err}");
        ensure_response_ca_matches_confirmed(&daemon_ca, &daemon_ca).unwrap();
    }

    #[tokio::test]
    async fn enroll_response_read_refuses_an_oversized_stream() {
        // A hostile endpoint (or relay in the path) can stream arbitrary bytes
        // before the response is trusted: the CA fetch runs before SAS
        // confirmation and the enroll POST before any cert is persisted. The
        // read must stop at the cap instead of growing the buffer.
        use tokio::io::AsyncWriteExt as _;
        let (client, mut server) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let junk = vec![b'A'; 8192];
            // Well past MAX_ENROLL_RESPONSE_BYTES; stop once the reader gives up.
            for _ in 0..64 {
                if server.write_all(&junk).await.is_err() {
                    break;
                }
            }
        });
        let mut client = client;
        let err = read_enroll_response(&mut client)
            .await
            .expect_err("an oversized enrollment response must be refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("exceeded") && msg.contains("refusing to buffer further"),
            "expected a size-cap refusal, got: {msg}"
        );
    }

    /// A listener that completes the TCP accept and then never speaks. The
    /// response deadline cannot help here: it starts only once a TLS session
    /// exists, and no handshake ever completes. Only the exchange budget ends
    /// this. The accepted sockets are held so the peer sees no EOF.
    async fn accept_then_silent_listener() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        (addr, server)
    }

    /// How long the exchange actually ran, on the paused clock. The clock
    /// auto-advances to the next deadline, so this is the budget the code really
    /// spent - a test that only asserts "an error came back" would pass just as
    /// happily against a budget of a day.
    fn assert_spent_the_budget(waited: tokio::time::Duration) {
        let budget = std::time::Duration::from_secs(ENROLL_EXCHANGE_TIMEOUT_SECS);
        // A LITERAL ceiling: bounds expressed only in terms of the constant
        // under test move with it, so widening the budget to a day would still
        // satisfy them. This one says what the budget is for.
        assert!(
            waited <= std::time::Duration::from_secs(60),
            "the exchange budget must stay human-scale: {waited:?}"
        );
        assert!(
            waited >= budget,
            "the exchange must run until its budget, not fail early: {waited:?}"
        );
        assert!(
            waited <= budget + std::time::Duration::from_secs(1),
            "the exchange must end AT its budget: {waited:?} against a {budget:?} budget"
        );
    }

    /// The CA preflight runs before anything is trusted, so a silent endpoint
    /// here stalls `zerocode --enroll` before the operator is ever prompted.
    #[tokio::test(start_paused = true)]
    async fn a_silent_endpoint_cannot_stall_the_trust_fetch() {
        let (addr, server) = accept_then_silent_listener().await;

        let started = tokio::time::Instant::now();
        let err = format!(
            "{:#}",
            fetch_enroll_trust(&addr.ip().to_string(), addr.port())
                .await
                .expect_err("a silent enrollment endpoint must not hold the client")
        );

        assert_spent_the_budget(started.elapsed());
        assert!(err.contains("did not complete within"), "got: {err}");
        assert!(
            err.contains(&ENROLL_EXCHANGE_TIMEOUT_SECS.to_string()),
            "the refusal must name the budget it spent, got: {err}"
        );
        server.abort();
    }

    /// The same guarantee on the POST exchange, which carries the pairing code
    /// and the CSR.
    #[tokio::test(start_paused = true)]
    async fn a_silent_endpoint_cannot_stall_the_enrollment_request() {
        let (addr, server) = accept_then_silent_listener().await;
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();

        let started = tokio::time::Instant::now();
        let err = format!(
            "{:#}",
            post_enroll(
                &addr.ip().to_string(),
                addr.port(),
                "270391",
                "csr",
                &daemon_ca
            )
            .await
            .expect_err("a silent enrollment endpoint must not hold the client")
        );

        assert_spent_the_budget(started.elapsed());
        assert!(err.contains("did not complete within"), "got: {err}");
        server.abort();
    }

    /// The relayed path dials a relay rather than the daemon, so it needs its own
    /// budget: a silent relay stalls in the outer WebSocket/TLS handshake, before
    /// the enrollment stream that the response deadline would cover exists.
    #[tokio::test(start_paused = true)]
    async fn a_silent_relay_cannot_stall_relayed_enrollment() {
        let (addr, server) = accept_then_silent_listener().await;
        let relay = crate::client::RelayDial {
            relay_addr: addr.to_string(),
            relay_host: "localhost".into(),
            node_id: "node-1".into(),
            relay_ca_path: None,
            relay_insecure: true,
            relay_pin: None,
            relay_tofu: false,
            pin_store: None,
            outer_client_cert: None,
            outer_client_key: None,
        };

        let started = tokio::time::Instant::now();
        let err = format!(
            "{:#}",
            fetch_enroll_trust_via_relay(&relay)
                .await
                .expect_err("a silent relay must not hold the client")
        );

        assert_spent_the_budget(started.elapsed());
        assert!(err.contains("did not complete within"), "got: {err}");
        assert!(err.contains("relay"), "got: {err}");
        server.abort();
    }

    /// A relay that admits the route and then goes inert leaves the enrollment
    /// exchange with nothing to complete: the inner TLS handshake is never
    /// answered. When the exchange is abandoned - its budget expiring, or the
    /// caller dropping it - the whole future is discarded and NO code the
    /// exchange wrote runs. Only the tunnel guard's destructor can retire the
    /// pump, and a detached pump would sit here holding the route while every
    /// retry added another.
    ///
    /// Real clock on purpose: a paused clock auto-advances through the setup
    /// budget while the TLS and WebSocket handshakes are still in flight, so the
    /// tunnel would never open and this test would prove nothing. Cancellation
    /// stands in for the budget expiring - it is the same discarded future, and
    /// it does not cost the test the full budget to observe.
    #[tokio::test]
    async fn an_abandoned_relay_enrollment_retires_its_pump() {
        let _serial = crate::client::live_pumps::exclusive().await;
        let (addr, opened, server) = relay_that_opens_then_stops_reading().await;
        let relay = crate::client::RelayDial {
            relay_addr: addr.to_string(),
            relay_host: "localhost".into(),
            node_id: "node-1".into(),
            relay_ca_path: None,
            relay_insecure: true,
            relay_pin: None,
            relay_tofu: false,
            pin_store: None,
            outer_client_cert: None,
            outer_client_key: None,
        };
        let before = crate::client::live_pumps::count();
        let completed_before = crate::client::live_pumps::completed();

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fetch_enroll_trust_via_relay(&relay),
        )
        .await;

        assert!(
            outcome.is_err(),
            "the exchange must still be in flight when it is abandoned"
        );
        // Non-vacuity: without this the test would also pass if the tunnel had
        // never opened and no pump had ever existed.
        assert!(
            opened.await.is_ok(),
            "the relay must have admitted the route, so a pump really existed"
        );

        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            crate::client::live_pumps::count(),
            before,
            "an abandoned exchange must leave no relay pump behind"
        );
        // And it must have been retired where it stood. A pump that merely
        // noticed its stream close would have run to the end of its body; one
        // parked in a relay write - the state this relay creates - never does.
        assert_eq!(
            crate::client::live_pumps::completed(),
            completed_before,
            "the tunnel guard must abort the pump, not rely on it noticing"
        );
        server.abort();
    }

    /// Echo the relay subprotocol so the client's handshake completes.
    ///
    /// The error type is tungstenite's `ErrorResponse`, which carries a whole
    /// HTTP response and so trips the large-error lint; the signature is the
    /// library's, and this handshake never takes the error path.
    #[allow(clippy::result_large_err)]
    fn echo_subprotocol(
        req: &tokio_tungstenite::tungstenite::handshake::server::Request,
        mut resp: tokio_tungstenite::tungstenite::handshake::server::Response,
    ) -> std::result::Result<
        tokio_tungstenite::tungstenite::handshake::server::Response,
        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
    > {
        if req
            .headers()
            .get("Sec-WebSocket-Protocol")
            .is_some_and(|v| {
                v.to_str()
                    .is_ok_and(|v| v.contains(crate::relay_proto::SUBPROTOCOL))
            })
        {
            resp.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                crate::relay_proto::SUBPROTOCOL
                    .parse()
                    .expect("static subprotocol"),
            );
        }
        Ok(resp)
    }

    /// A relay that completes its handshakes, admits the enrollment route, and
    /// then reads nothing further.
    async fn relay_that_opens_then_stops_reading() -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
        use futures_util::{SinkExt as _, StreamExt as _};
        use tokio_tungstenite::tungstenite::Message;

        let (_, ca, ca_key) = crate::client_crypto::test_pki::gen_ca();
        let (cert, key) =
            crate::client_crypto::test_pki::gen_server_cert(&ca, &ca_key, &["localhost".into()]);
        let acceptor = crate::client_crypto::test_pki::tls_acceptor(&cert, &key);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (opened_tx, opened_rx) = tokio::sync::oneshot::channel();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let tls = acceptor.accept(tcp).await.expect("relay outer TLS");
            let mut ws = tokio_tungstenite::accept_hdr_async(tls, echo_subprotocol)
                .await
                .expect("relay ws accept");
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(t) = msg
                    && matches!(
                        crate::relay_proto::Control::from_json(t.as_str()),
                        Ok(crate::relay_proto::Control::Enroll { .. })
                    )
                {
                    break;
                }
            }
            ws.send(Message::text(
                crate::relay_proto::Control::Opened { conn_id: 7 }.to_json(),
            ))
            .await
            .expect("send Opened");
            let _ = opened_tx.send(());
            std::future::pending::<()>().await;
        });
        (addr, opened_rx, server)
    }

    #[tokio::test(start_paused = true)]
    async fn enroll_response_read_times_out_on_a_stalled_stream() {
        // A peer that opens the stream and then sends nothing must not hold the
        // client forever. Time is paused so the deadline fires without the test
        // actually waiting ENROLL_READ_TIMEOUT_SECS.
        let (client, _server) = tokio::io::duplex(8192);
        let mut client = client;
        let err = read_enroll_response(&mut client)
            .await
            .expect_err("a stalled enrollment response must time out");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out"),
            "expected a deadline error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn post_enroll_refuses_unconfirmed_ca_before_sending_code_or_csr() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (confirmed_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
        let (_, rogue_ca, rogue_key) = crate::client_crypto::test_pki::gen_ca();
        let (server_cert, server_key) = crate::client_crypto::test_pki::gen_server_cert(
            &rogue_ca,
            &rogue_key,
            &["localhost".into()],
        );
        let acceptor = crate::client_crypto::test_pki::tls_acceptor(&server_cert, &server_key);
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
        let (daemon_ca, _, _) = crate::client_crypto::test_pki::gen_ca();
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
