//! Certificate enrollment endpoint - the bootstrap surface a certless client
//! reaches for its FIRST client certificate.
//!
//! This is deliberately NOT a fallback on the always-mTLS RPC plane (that plane
//! stays mutually authenticated with no weakenable path). It is a separate,
//! minimal, server-authenticated-TLS endpoint with its own auth model:
//!
//! 1. The client opens provisional server-auth TLS only to fetch the daemon CA.
//! 2. The operator confirms that CA out of band via the pairing short-auth-string
//!    [`zeroclaw_tls::enrollment_sas`], then the client reconnects pinned to that
//!    confirmed CA.
//! 3. Only after that trust step does the client submit a pairing code + CSR. The CA
//!    reads ONLY the CSR public key (the private key never leaves the device) and
//!    signs a `clientAuth`-only leaf bound to a daemon-minted device id
//!    ([`zeroclaw_tls::sign_csr`]).
//! 4. The daemon records the issuance in its ledger + audit trail and returns the
//!    signed cert + CA chain + the relay profile, so the client can immediately open
//!    the mutually authenticated RPC plane (directly or via the relay).
//!
//! The daemon owns the CA, so this endpoint works with no gateway. The HTTP is
//! hand-rolled (one fixed route) to keep the runtime free of any gateway-shaped
//! web-framework dependency.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::pairing::PairingGuard;

use crate::security::cert_ledger::{CertLedger, CertStatus, IssuanceActor, LedgerEntry};

/// Maximum bytes accepted for an enrollment request (headers + body). A CSR is a
/// few KiB; this is a generous cap that still bounds a memory-exhaustion attempt.
const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Per-connection deadline for the whole TLS + request/response exchange.
const CONN_TIMEOUT_SECS: u64 = 15;
/// Concurrent in-flight enrollment connections (bounds handshake/signing load).
const MAX_INFLIGHT: usize = 16;

/// Routing target a freshly enrolled client should use to reach this daemon
/// through a relay. Delivered in the enrollment response so the client is
/// zero-config on its next run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RelayProfile {
    /// Relay address (`host:port`) to dial.
    pub relay_url: String,
    /// Opaque node-id naming this daemon on the relay.
    pub node_id: String,
    /// SHA-256 of the relay's OUTER leaf certificate to pin (empty if unknown
    /// yet; the client may then TOFU-pin or be given `--relay-pin`).
    pub relay_cert_pin: String,
}

/// Assemble the relay coordinates handed to an enrolling or renewing client.
/// Default (empty) when no relay is configured. The pin is the relay's OUTER leaf
/// fingerprint, sourced from the relay bridge's pin store when it exists.
pub fn relay_profile(
    data_dir: &std::path::Path,
    relay: &zeroclaw_config::schema::RelayConfig,
) -> RelayProfile {
    if relay.enabled && !relay.url.is_empty() {
        let node_id = crate::relay::ensure_node_id(data_dir, &relay.node_id)
            .unwrap_or_else(|_| relay.node_id.clone());
        let relay_cert_pin = std::fs::read_to_string(data_dir.join("relay").join("relay_pin"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        RelayProfile {
            relay_url: relay.url.clone(),
            node_id,
            relay_cert_pin,
        }
    } else {
        RelayProfile::default()
    }
}

/// The enrollment request body (`POST /enroll`).
#[derive(Debug, Deserialize)]
struct EnrollRequest {
    /// One-time pairing code. Required for the first FOSS release; the reserved
    /// code-less migration knob is rejected at daemon startup.
    #[serde(default)]
    pairing_code: String,
    /// PEM-encoded PKCS#10 certificate signing request. The CA reads only its
    /// public key; the device id is daemon-assigned, not taken from the CSR.
    csr_pem: String,
}

/// The enrollment response body (200).
#[derive(Debug, Serialize)]
struct EnrollResponse {
    /// The signed client certificate (PEM).
    cert_pem: String,
    /// The daemon CA chain (PEM) the client pins for the RPC plane.
    ca_chain_pem: String,
    /// The daemon-assigned stable device id (cert subject CN / ledger key).
    device_id: String,
    /// Certificate `notAfter` (unix seconds) so the client can schedule renewal
    /// at ~50% of the TTL.
    not_after: i64,
    /// Where to reach this daemon through a relay (empty fields when no relay).
    relay_profile: RelayProfile,
}

/// The preflight response body (`GET /enroll/ca`). It intentionally contains only
/// the daemon CA needed for SAS confirmation; it does not consume or receive a
/// pairing code.
#[derive(Debug, Serialize)]
struct EnrollTrustResponse {
    ca_chain_pem: String,
}

/// Everything the enrollment endpoint needs to serve requests.
pub struct EnrollServer {
    pub bind_addr: SocketAddr,
    /// Server-authentication-only TLS acceptor (no client cert; this is the
    /// bootstrap surface, not the mTLS RPC plane).
    pub acceptor: tokio_rustls::TlsAcceptor,
    /// CA cert PEM (returned to the client and used for the SAS fingerprint).
    pub ca_cert_pem: String,
    /// CA key PEM (decrypted), used only to sign CSRs. Held in memory for the
    /// endpoint's lifetime; never written or returned.
    pub ca_key_pem: zeroize::Zeroizing<String>,
    pub ledger: Arc<CertLedger>,
    pub pairing: Arc<PairingGuard>,
    /// Static WSS pin allowlists cannot admit freshly issued in-band certs unless
    /// the operator updates the canonical pin source out of band.
    pub static_client_pins_configured: bool,
    /// RFC3339 instant before which a code-less ("unpaired") enrollment is
    /// accepted - the time-boxed migration window. `None` means closed.
    pub allow_unpaired_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Relay coordinates to hand back, when the relay bridge is configured.
    pub relay_profile: RelayProfile,
}

impl EnrollServer {
    /// Authenticate, sign, record, and build the response. Returns `(status,
    /// json_error)` on any failure so the caller writes a clean HTTP error.
    async fn process(
        &self,
        req: &EnrollRequest,
        peer: &str,
    ) -> Result<EnrollResponse, (u16, String)> {
        // 1. Authenticate: the pairing code is consumed and brute-force protected
        //    before any certificate is signed.
        let pairing_code = req.pairing_code.trim();
        if self.static_client_pins_configured {
            return Err((
                409,
                "in-band enrollment is disabled because [wss.client_auth].pinned_certs is \
                 configured; provision pinned client certificates out of band"
                    .to_string(),
            ));
        }
        if pairing_code.is_empty() {
            return Err((
                401,
                "a pairing code is required; ask the operator for the daemon enrollment code"
                    .to_string(),
            ));
        }
        let pairing = match self.pairing.reserve_pair(pairing_code, peer).await {
            Ok(Some(pairing)) => pairing,
            Ok(None) => {
                return Err((401, "invalid or already-used pairing code".to_string()));
            }
            Err(secs) => {
                return Err((429, format!("too many attempts; retry in {secs}s")));
            }
        };
        let token_hash = pairing.token_hash();

        // 2. The daemon assigns the device identity (never the client/CSR): a
        //    stable, unguessable id that becomes the cert CN and the ledger key.
        let device_id = mint_device_id();

        // 3. Sign the CSR. sign_csr reads ONLY the CSR public key and stamps the
        //    daemon's clientAuth-only profile, so CSR-supplied fields are ignored.
        let issued = zeroclaw_tls::sign_csr(
            &self.ca_cert_pem,
            &self.ca_key_pem,
            &device_id,
            &req.csr_pem,
        )
        .map_err(|e| (400, format!("CSR rejected: {e}")))?;

        // 4. Record the issuance in the ledger + append-only audit trail.
        let actor = IssuanceActor::Enrollment {
            token_hash: token_hash.clone(),
        };
        let entry = LedgerEntry {
            device_id: device_id.clone(),
            fingerprint: issued.fingerprint.clone(),
            not_before: issued.not_before,
            not_after: issued.not_after,
            status: CertStatus::Active,
            token_hash,
            actor: actor.label(),
            issued_at: now_unix(),
        };
        self.ledger
            .record_issued(&entry, false)
            .map_err(|e| (500, format!("ledger error: {e}")))?;
        pairing.commit();

        Ok(EnrollResponse {
            cert_pem: issued.cert_pem,
            ca_chain_pem: self.ca_cert_pem.clone(),
            device_id,
            not_after: issued.not_after,
            relay_profile: self.relay_profile.clone(),
        })
    }
}

/// Run the enrollment endpoint until `cancel` fires.
pub async fn serve(server: Arc<EnrollServer>, cancel: CancellationToken) -> Result<()> {
    let listener = TcpListener::bind(server.bind_addr)
        .await
        .with_context(|| format!("bind enrollment endpoint on {}", server.bind_addr))?;
    serve_on(listener, server, cancel).await
}

/// Run the enrollment endpoint on a pre-bound listener (used by tests so they can
/// bind `127.0.0.1:0` and learn the assigned port).
pub async fn serve_on(
    listener: TcpListener,
    server: Arc<EnrollServer>,
    cancel: CancellationToken,
) -> Result<()> {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({ "bind": server.bind_addr.to_string() })),
        "enrollment endpoint listening"
    );
    let inflight = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(v) => v,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                            &format!("enrollment accept error: {e}")
                        );
                        continue;
                    }
                };
                let Ok(permit) = inflight.clone().try_acquire_owned() else {
                    // At capacity: drop the connection rather than queue unbounded work.
                    continue;
                };
                let server = server.clone();
                zeroclaw_spawn::spawn!(async move {
                    let _permit = permit;
                    let peer_ip = peer.ip().to_string();
                    let fut = handle_conn(&server, tcp, &peer_ip);
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(CONN_TIMEOUT_SECS),
                        fut,
                    )
                    .await;
                });
            }
        }
    }
}

async fn handle_conn(server: &EnrollServer, tcp: tokio::net::TcpStream, peer: &str) {
    let mut tls = match server.acceptor.accept(tcp).await {
        Ok(s) => s,
        Err(_) => return, // not a TLS client / handshake failure
    };
    let (method, path, body) = match read_request(&mut tls, MAX_REQUEST_BYTES).await {
        Ok(v) => v,
        Err(_) => {
            let _ = write_json(
                &mut tls,
                400,
                "Bad Request",
                b"{\"error\":\"malformed request\"}",
            )
            .await;
            return;
        }
    };
    if method == "GET" && path == "/enroll/ca" {
        let json = serde_json::to_vec(&EnrollTrustResponse {
            ca_chain_pem: server.ca_cert_pem.clone(),
        })
        .unwrap_or_default();
        let _ = write_json(&mut tls, 200, "OK", &json).await;
        return;
    }
    if method != "POST" || path != "/enroll" {
        let _ = write_json(&mut tls, 404, "Not Found", b"{\"error\":\"unknown route\"}").await;
        return;
    }
    let req: EnrollRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => {
            let _ = write_json(
                &mut tls,
                400,
                "Bad Request",
                b"{\"error\":\"invalid JSON body\"}",
            )
            .await;
            return;
        }
    };
    match server.process(&req, peer).await {
        Ok(resp) => {
            let json = serde_json::to_vec(&resp).unwrap_or_default();
            let _ = write_json(&mut tls, 200, "OK", &json).await;
        }
        Err((status, msg)) => {
            let reason = http_reason(status);
            let body = serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default();
            let _ = write_json(&mut tls, status, reason, &body).await;
        }
    }
}

fn http_reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Error",
    }
}

/// Mint a daemon-controlled stable device id (`dev_<16 hex>`, 64 bits of entropy).
fn mint_device_id() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    // SystemRandom is the same CSPRNG the relay node-id path uses.
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        // Extremely unlikely; fall back to a time-seeded id so we never panic.
        let t = now_unix() as u64;
        bytes.copy_from_slice(&t.to_be_bytes());
    }
    format!("dev_{}", hex::encode(bytes))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read one HTTP/1.1 request: `(method, path, body)`. Bounded by `max` bytes.
async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    max: usize,
) -> Result<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    // Read until the header terminator is seen.
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > max {
            anyhow::bail!("request headers exceed {max} bytes");
        }
        let n = stream
            .read(&mut tmp)
            .await
            .context("read request headers")?;
        if n == 0 {
            anyhow::bail!("connection closed before request headers");
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    for line in lines {
        if let Some(v) = line
            .split_once(':')
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.trim())
        {
            content_length = v.parse().unwrap_or(0);
        }
    }
    if content_length > max {
        anyhow::bail!("request body exceeds {max} bytes");
    }

    let body_start = header_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await.context("read request body")?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > max {
            anyhow::bail!("request body exceeds {max} bytes");
        }
    }
    body.truncate(content_length);
    Ok((method, path, body))
}

async fn write_json<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    reason: &str,
    body: &[u8],
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    // Send TLS close_notify (and close the write side) so the client's
    // read-to-end completes cleanly rather than seeing a truncated stream.
    let _ = stream.shutdown().await;
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_request_parses_method_path_and_body() {
        let raw = b"POST /enroll HTTP/1.1\r\nHost: x\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let (m, p, body) = read_request(&mut cursor, 1024).await.unwrap();
        assert_eq!(m, "POST");
        assert_eq!(p, "/enroll");
        assert_eq!(body, b"{\"a\":1}");
    }

    #[tokio::test]
    async fn read_request_rejects_oversized_body() {
        let raw = b"POST /enroll HTTP/1.1\r\nContent-Length: 9999\r\n\r\n";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        assert!(read_request(&mut cursor, 64).await.is_err());
    }

    #[test]
    fn mint_device_id_shape() {
        let id = mint_device_id();
        assert!(id.starts_with("dev_"));
        assert_eq!(id.len(), 4 + 16);
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(mint_device_id(), mint_device_id());
    }

    fn test_server(
        pairing: PairingGuard,
        deadline: Option<chrono::DateTime<chrono::Utc>>,
    ) -> EnrollServer {
        test_server_with_ledger(
            pairing,
            deadline,
            Arc::new(CertLedger::open_in_memory(None).unwrap()),
        )
    }

    fn test_server_with_ledger(
        pairing: PairingGuard,
        deadline: Option<chrono::DateTime<chrono::Utc>>,
        ledger: Arc<CertLedger>,
    ) -> EnrollServer {
        let (ca_cert, ca_key) = zeroclaw_tls::testing::gen_ca();
        // A throwaway server-auth acceptor (not exercised by process()).
        let (srv_cert, srv_key) =
            zeroclaw_tls::testing::gen_server_cert(&ca_cert, &ca_key, &["localhost".into()]);
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("s.crt");
        let key_path = dir.path().join("s.key");
        std::fs::write(&cert_path, &srv_cert).unwrap();
        std::fs::write(&key_path, &srv_key).unwrap();
        let acceptor = zeroclaw_tls::build_tls_acceptor(&zeroclaw_tls::ServerConfigParams {
            cert_path: cert_path.to_string_lossy().into_owned(),
            key_path: key_path.to_string_lossy().into_owned(),
            client_auth: None,
        })
        .unwrap();
        EnrollServer {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            acceptor,
            ca_cert_pem: ca_cert,
            ca_key_pem: zeroize::Zeroizing::new(ca_key),
            ledger,
            pairing: Arc::new(pairing),
            static_client_pins_configured: false,
            allow_unpaired_until: deadline,
            relay_profile: RelayProfile::default(),
        }
    }

    #[tokio::test]
    async fn process_issues_cert_with_valid_code_and_ignores_csr_identity() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let code = pairing.pairing_code().unwrap();
        let server = test_server(pairing, None);
        // A CSR requesting an attacker CN; the daemon must mint its own device id.
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("attacker-wants-this");
        let req = EnrollRequest {
            pairing_code: code,
            csr_pem: csr,
        };
        let resp = server.process(&req, "1.2.3.4").await.unwrap();
        assert!(resp.device_id.starts_with("dev_"));
        assert!(resp.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(resp.ca_chain_pem.contains("BEGIN CERTIFICATE"));
        // The issued cert is recorded active in the ledger.
        let fps = server.ledger.list_active().unwrap();
        assert_eq!(fps.len(), 1);
        assert_eq!(fps[0].device_id, resp.device_id);
    }

    #[tokio::test]
    async fn process_propagates_certificate_audit_write_failure() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let audit = crate::security::audit::AuditLogger::new(
            zeroclaw_config::schema::AuditConfig {
                enabled: true,
                log_path: "missing/audit.log".to_string(),
                max_size_mb: 100,
                sign_events: false,
            },
            dir.path().to_path_buf(),
        )
        .unwrap();
        let ledger = Arc::new(CertLedger::open_in_memory(Some(Arc::new(audit))).unwrap());
        let pairing = PairingGuard::new(true, &[]);
        let code = pairing.pairing_code().unwrap();
        let server = test_server_with_ledger(pairing, None, ledger);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: code,
            csr_pem: csr,
        };

        let err = server.process(&req, "1.2.3.4").await.unwrap_err();
        assert_eq!(err.0, 500);
        assert!(err.1.contains("certificate audit event"), "got: {}", err.1);
        assert!(
            server.pairing.pairing_code().is_some(),
            "ledger/audit failure must not consume the one-time pairing code"
        );
    }

    #[tokio::test]
    async fn process_rejects_in_band_enrollment_when_static_pins_are_configured() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let code = pairing.pairing_code().unwrap();
        let mut server = test_server(pairing, None);
        server.static_client_pins_configured = true;
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: code.clone(),
            csr_pem: csr,
        };

        let err = server.process(&req, "1.2.3.4").await.unwrap_err();
        assert_eq!(err.0, 409);
        assert!(err.1.contains("pinned_certs"), "got: {}", err.1);
        assert_eq!(
            server.pairing.pairing_code().as_deref(),
            Some(code.as_str()),
            "static-pin refusal must not consume the one-time pairing code"
        );
    }

    #[tokio::test]
    async fn process_allows_retry_after_valid_code_with_bad_csr() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let code = pairing.pairing_code().unwrap();
        let server = test_server(pairing, None);
        let bad = EnrollRequest {
            pairing_code: code.clone(),
            csr_pem: "not a csr".to_string(),
        };

        let err = server.process(&bad, "1.2.3.4").await.unwrap_err();
        assert_eq!(err.0, 400);
        assert_eq!(
            server.pairing.pairing_code().as_deref(),
            Some(code.as_str()),
            "CSR rejection must leave the one-time code retryable"
        );

        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let good = EnrollRequest {
            pairing_code: code,
            csr_pem: csr,
        };
        assert!(
            server.process(&good, "1.2.3.4").await.is_ok(),
            "the restored code should work on retry"
        );
    }

    #[tokio::test]
    async fn process_rejects_wrong_code_and_consumes_one_time_code() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let server = test_server(pairing, None);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let bad = EnrollRequest {
            pairing_code: "000000".to_string(),
            csr_pem: csr,
        };
        let err = server.process(&bad, "1.2.3.4").await.unwrap_err();
        assert_eq!(err.0, 401);
        assert!(server.ledger.list_active().unwrap().is_empty());
    }

    #[tokio::test]
    async fn process_requires_code_when_window_closed() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let server = test_server(pairing, None);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: String::new(),
            csr_pem: csr,
        };
        let err = server.process(&req, "1.2.3.4").await.unwrap_err();
        assert_eq!(err.0, 401);
    }

    #[tokio::test]
    async fn over_the_wire_enroll_issues_a_cert_through_tls() {
        // Make-or-break: a real TLS client POSTs a CSR over the server-auth
        // enrollment endpoint and gets back a 200 with a signed certificate.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let code = pairing.pairing_code().unwrap();
        let server = test_server(pairing, None);
        let ca_pem = server.ca_cert_pem.clone();
        let server = Arc::new(server);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server_c = server.clone();
        let cancel_c = cancel.clone();
        let srv =
            zeroclaw_spawn::spawn!(async move { serve_on(listener, server_c, cancel_c).await });

        // Client trusts the test CA (server-auth only) and dials "localhost"
        // (the server cert SAN) at the loopback address.
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &ca_pem).unwrap();
        let ca_ders = zeroclaw_tls::load_certs(&ca_file.path().to_string_lossy()).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        for c in ca_ders {
            roots.add(c).unwrap();
        }
        let client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));

        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        let req = "GET /enroll/ca HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
        tls.write_all(req.as_bytes()).await.unwrap();
        tls.flush().await.unwrap();
        let mut ca_resp = Vec::new();
        tls.read_to_end(&mut ca_resp).await.unwrap();
        let ca_text = String::from_utf8_lossy(&ca_resp);
        assert!(
            ca_text.starts_with("HTTP/1.1 200") && ca_text.contains("ca_chain_pem"),
            "expected CA preflight 200, got: {ca_text}"
        );

        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();

        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("ignored-by-daemon");
        let body = serde_json::json!({ "pairing_code": code, "csr_pem": csr }).to_string();
        let req = format!(
            "POST /enroll HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        tls.write_all(req.as_bytes()).await.unwrap();
        tls.flush().await.unwrap();

        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(
            text.starts_with("HTTP/1.1 200"),
            "expected 200, got: {text}"
        );
        assert!(
            text.contains("BEGIN CERTIFICATE"),
            "no cert in response: {text}"
        );
        assert!(text.contains("device_id"), "no device_id in response");
        // The issuance was recorded in the ledger.
        assert_eq!(server.ledger.list_active().unwrap().len(), 1);

        cancel.cancel();
        let _ = srv.await;
    }

    #[tokio::test]
    async fn process_rejects_codeless_enroll_even_when_reserved_window_is_configured() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[]);
        let future = chrono::Utc::now() + chrono::Duration::minutes(10);
        let server = test_server(pairing, Some(future));
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: String::new(),
            csr_pem: csr,
        };
        let err = server.process(&req, "1.2.3.4").await.unwrap_err();
        assert_eq!(err.0, 401);
        assert!(server.ledger.list_active().unwrap().is_empty());
    }
}
