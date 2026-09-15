//! The relay half of browser enrollment: perform the daemon's native enrollment
//! exchange on a browser's behalf.
//!
//! This mirrors `apps/zerocode/src/enroll.rs` deliberately and literally. The
//! daemon end is unchanged, so the wire exchange has to be the one it already
//! serves:
//!
//! 1. open a route to the daemon's enrollment endpoint (`crate::enroll_route`),
//! 2. TLS with a provisional (accept-any) verifier, `GET /enroll/ca`,
//! 3. the OPERATOR confirms that CA out of band by comparing the SAS,
//! 4. a SECOND route and a SECOND TLS session, this time pinned to the confirmed
//!    CA, carrying `POST /enroll`,
//! 5. reject the response if it returns a different CA than the confirmed one.
//!
//! Steps 2 and 4 are separate connections in the native client too - the pin can
//! only be applied to a handshake that has not happened yet, and the human sits
//! between them.
//!
//! TRUST: the relay sees the pairing code and the issued certificate in the
//! clear here. It does NOT see the private key: the browser generates the
//! keypair and sends only a CSR. A relay that wanted to could still mint its own
//! certificate with a code it observed, so this path is disclosed as
//! relay-terminated rather than presented as end-to-end. Nothing in this module
//! is on the zerocode/native enrollment path.

use crate::enroll_route::{OpenError, open_enroll_route};
use crate::{Inner, MAX_NODE_ID_LEN};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// TLS server name for the daemon's enrollment endpoint reached through a relay
/// route. The daemon's enrollment leaf is issued for its loopback bind, and the
/// route lands on that same socket, so this is the name the pinned leg must
/// verify against. Must stay identical to zerocode's `RELAY_ENROLL_SERVER_NAME`;
/// the two clients validate the same certificate.
const RELAY_ENROLL_SERVER_NAME: &str = "127.0.0.1";

/// Largest enrollment response the relay will buffer from a daemon. An
/// enrollment reply is a certificate, a CA chain and a small relay profile. The
/// daemon bounds its own side of the exchange at 64 KiB and the native client
/// mirrors that figure; this is the same bound at the third party in the path.
///
/// Without it the daemon side of this route is an unbounded read into relay
/// memory, which is what the browser frontdoor's predecessor did in JavaScript.
const MAX_ENROLL_RESPONSE_BYTES: usize = 64 * 1024;

/// Largest request body the frontdoor accepts from a browser. A CSR plus a
/// pairing code plus a confirmed CA is a few KiB; this mirrors the daemon's own
/// `MAX_REQUEST_BYTES`.
pub(crate) const MAX_FRONTDOOR_REQUEST_BYTES: usize = 64 * 1024;

/// Ceiling on reading one leg's response, mirroring the daemon's per-connection
/// timeout so a stalled daemon cannot hold a relay task and a route open.
const ENROLL_READ_TIMEOUT_SECS: u64 = 15;

/// Ceiling on one whole leg: route open, TLS handshake, request write, response
/// read. Per-leg rather than per-exchange because the operator's SAS comparison
/// sits between the two legs and a human has no deadline - the same reasoning
/// (and the same figure) as the native client's per-exchange budget.
const ENROLL_LEG_TIMEOUT_SECS: u64 = 30;

/// What the page needs to display the SAS. Only the CA - no pairing code has
/// been sent and nothing has been trusted yet.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrustReply {
    pub(crate) ca_chain_pem: String,
}

/// A browser's enrollment request. `ca_chain_pem` is the CA the OPERATOR
/// confirmed by SAS in step 3; the relay pins leg 2 to it rather than to
/// whatever the daemon offers, so a daemon that answers the second connection
/// with a different identity is refused.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollBody {
    pub(crate) node_id: String,
    pub(crate) pairing_code: String,
    pub(crate) csr_pem: String,
    pub(crate) ca_chain_pem: String,
}

/// A node-id request for the trust preflight.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrustBody {
    pub(crate) node_id: String,
}

/// Refusals the frontdoor turns into HTTP statuses. `Upstream` carries the
/// daemon's own status so a wrong pairing code still reads as 401 to the page.
pub(crate) enum ProxyError {
    Route(OpenError),
    BadRequest(String),
    Upstream(u16, String),
    Exchange(String),
}

impl ProxyError {
    pub(crate) fn status(&self) -> u16 {
        match self {
            Self::Route(OpenError::NoSuchNode) => 404,
            Self::Route(OpenError::RateLimited) => 429,
            Self::Route(OpenError::Busy) => 503,
            Self::Route(OpenError::NotAccepted) => 504,
            Self::BadRequest(_) => 400,
            Self::Upstream(code, _) => *code,
            Self::Exchange(_) => 502,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Route(e) => e.as_str().to_string(),
            Self::BadRequest(m) | Self::Upstream(_, m) | Self::Exchange(m) => m.clone(),
        }
    }
}

/// Reject a node-id before it reaches the registry, on the same rules the
/// registry itself enforces.
fn check_node_id(node_id: &str) -> Result<(), ProxyError> {
    if node_id.is_empty() || node_id.len() > MAX_NODE_ID_LEN {
        return Err(ProxyError::BadRequest("invalid node id".into()));
    }
    if !node_id.chars().all(|c| c.is_ascii_graphic()) {
        return Err(ProxyError::BadRequest("invalid node id".into()));
    }
    Ok(())
}

/// Leg 1: fetch the daemon CA over provisional TLS so the page can show its SAS.
///
/// Sends no pairing code and no CSR. Nothing here is trusted yet - that is the
/// operator's job in step 3.
pub(crate) async fn fetch_trust(
    inner: &Arc<Inner>,
    node_id: &str,
) -> Result<TrustReply, ProxyError> {
    check_node_id(node_id)?;
    within_leg_budget("the enrollment trust fetch", async {
        let route = open_enroll_route(inner, node_id)
            .await
            .map_err(ProxyError::Route)?;
        let (stream, _pump) = route.split();
        let mut tls = connect_tls(
            stream,
            provisional_config(),
            "enrollment trust TLS handshake",
        )
        .await
        .map_err(|e| ProxyError::Exchange(e.to_string()))?;

        let request = format!(
            "GET /enroll/ca HTTP/1.1\r\nHost: {RELAY_ENROLL_SERVER_NAME}\r\nConnection: close\r\n\r\n"
        );
        write_request(&mut tls, request.as_bytes())
            .await
            .map_err(|e| ProxyError::Exchange(e.to_string()))?;
        let raw = read_bounded(&mut tls)
            .await
            .map_err(|e| ProxyError::Exchange(e.to_string()))?;
        let (status, body) =
            split_http(&raw).map_err(|e| ProxyError::Exchange(e.to_string()))?;
        if status != 200 {
            return Err(ProxyError::Upstream(
                status,
                "enrollment endpoint refused the trust preflight".into(),
            ));
        }
        let parsed: TrustReply = serde_json::from_slice(body)
            .map_err(|_| ProxyError::Exchange("malformed trust response".into()))?;
        // A CA that will not parse cannot be pinned in leg 2 and cannot produce
        // a meaningful SAS, so fail here rather than showing the operator a
        // digest of something that is not a certificate.
        single_ca_der(&parsed.ca_chain_pem)
            .map_err(|e| ProxyError::Exchange(e.to_string()))?;
        Ok(parsed)
    })
    .await
}

/// Leg 2: POST the browser's CSR over TLS pinned to the operator-confirmed CA.
///
/// Returns the daemon's response body verbatim. The relay does not rewrite it:
/// the page needs `cert_pem`, `device_id`, `not_after` and the relay profile
/// exactly as the daemon issued them.
pub(crate) async fn post_enroll(
    inner: &Arc<Inner>,
    body: &EnrollBody,
) -> Result<serde_json::Value, ProxyError> {
    check_node_id(&body.node_id)?;
    if body.pairing_code.is_empty() {
        return Err(ProxyError::BadRequest("a pairing code is required".into()));
    }
    if body.csr_pem.is_empty() {
        return Err(ProxyError::BadRequest("a CSR is required".into()));
    }
    let confirmed = single_ca_der(&body.ca_chain_pem)
        .map_err(|_| ProxyError::BadRequest("confirmed CA is not a single certificate".into()))?;
    let confirmed_fpr = zeroclaw_tls::cert_sha256_fingerprint(confirmed.as_ref());

    within_leg_budget("the enrollment request", async {
        let route = open_enroll_route(inner, &body.node_id)
            .await
            .map_err(ProxyError::Route)?;
        let (stream, _pump) = route.split();
        let config = pinned_config(&body.ca_chain_pem)
            .map_err(|e| ProxyError::BadRequest(e.to_string()))?;
        let mut tls = connect_tls(
            stream,
            config,
            "enrollment TLS handshake with confirmed daemon CA",
        )
        .await
        .map_err(|e| ProxyError::Exchange(e.to_string()))?;

        // Only the fields the daemon's `EnrollRequest` declares. The pairing
        // code is in this buffer; it is never logged.
        let payload = serde_json::json!({
            "pairing_code": body.pairing_code,
            "csr_pem": body.csr_pem,
        })
        .to_string();
        let request = format!(
            "POST /enroll HTTP/1.1\r\nHost: {RELAY_ENROLL_SERVER_NAME}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        write_request(&mut tls, request.as_bytes())
            .await
            .map_err(|e| ProxyError::Exchange(e.to_string()))?;
        let raw = read_bounded(&mut tls)
            .await
            .map_err(|e| ProxyError::Exchange(e.to_string()))?;
        let (status, raw_body) =
            split_http(&raw).map_err(|e| ProxyError::Exchange(e.to_string()))?;
        if status != 200 {
            let detail = serde_json::from_slice::<serde_json::Value>(raw_body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| "enrollment refused".to_string());
            return Err(ProxyError::Upstream(status, detail));
        }
        let parsed: serde_json::Value = serde_json::from_slice(raw_body)
            .map_err(|_| ProxyError::Exchange("malformed enrollment response".into()))?;

        // The response must come back under the SAME CA the operator confirmed.
        // The pinned handshake already proves the peer holds a key that CA
        // issued; this catches a daemon that then hands the client a different
        // CA to trust for the RPC plane. Mirrors the native client's check.
        let returned = parsed
            .get("ca_chain_pem")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProxyError::Exchange("enrollment response has no CA chain".into()))?;
        let returned_der =
            single_ca_der(returned).map_err(|e| ProxyError::Exchange(e.to_string()))?;
        if zeroclaw_tls::cert_sha256_fingerprint(returned_der.as_ref()) != confirmed_fpr {
            return Err(ProxyError::Exchange(
                "enrollment response returned a different CA than the one confirmed by SAS".into(),
            ));
        }
        Ok(parsed)
    })
    .await
}

async fn within_leg_budget<T>(
    what: &str,
    leg: impl std::future::Future<Output = Result<T, ProxyError>>,
) -> Result<T, ProxyError> {
    match tokio::time::timeout(std::time::Duration::from_secs(ENROLL_LEG_TIMEOUT_SECS), leg).await {
        Ok(result) => result,
        Err(_) => Err(ProxyError::Exchange(format!(
            "{what} did not complete within {ENROLL_LEG_TIMEOUT_SECS}s"
        ))),
    }
}

/// Accept any server certificate. Used ONLY for the preflight that fetches the
/// CA the operator is about to confirm: there is no trust anchor yet, and the
/// SAS comparison - not this handshake - is what establishes it. No pairing code
/// or CSR is ever sent over a provisional session.
#[derive(Debug)]
struct AcceptProvisional;

impl rustls::client::danger::ServerCertVerifier for AcceptProvisional {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
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

fn provisional_config() -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptProvisional))
        .with_no_client_auth()
}

fn pinned_config(ca_chain_pem: &str) -> Result<rustls::ClientConfig> {
    let ca_der = single_ca_der(ca_chain_pem)?;
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

/// The one certificate in a daemon CA chain. Exactly one: a chain with more than
/// one certificate has no single SAS for the operator to compare, so it is
/// refused rather than silently fingerprinting the first.
fn single_ca_der(ca_chain_pem: &str) -> Result<rustls::pki_types::CertificateDer<'static>> {
    let certs = rustls_pemfile::certs(&mut ca_chain_pem.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parsing daemon CA chain")?;
    match certs.len() {
        1 => Ok(certs.into_iter().next().expect("length checked")),
        n => anyhow::bail!("daemon CA chain must contain exactly one certificate (got {n})"),
    }
}

async fn connect_tls<S>(
    stream: S,
    config: rustls::ClientConfig,
    context: &'static str,
) -> Result<tokio_rustls::client::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from(RELAY_ENROLL_SERVER_NAME.to_string())
        .context("enrollment server name")?;
    connector
        .connect(server_name, stream)
        .await
        .context(context)
}

async fn write_request<S>(tls: &mut S, bytes: &[u8]) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tls.write_all(bytes)
        .await
        .context("write enrollment request")?;
    tls.flush().await.context("flush enrollment request")?;
    Ok(())
}

/// Read one enrollment response under BOTH a byte cap and a deadline.
///
/// `read_to_end` is unbounded in both dimensions, and this runs before anything
/// is trusted or delivered: a hostile or broken daemon on the far end of the
/// route could stream arbitrary bytes into relay memory, or simply stall and pin
/// the route. Mirrors the native client's bound at the third party in the path.
async fn read_bounded<S>(tls: &mut S) -> Result<Vec<u8>>
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
            "enrollment response timed out after {ENROLL_READ_TIMEOUT_SECS}s; the endpoint \
             stopped sending or is stalling the stream"
        ),
    }
}

/// Split an HTTP/1.1 response into its status code and body.
fn split_http(raw: &[u8]) -> Result<(u16, &[u8])> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("enrollment response has no header terminator")?;
    let head = std::str::from_utf8(&raw[..split]).context("enrollment response head")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("enrollment response has no status code")?;
    Ok((status, &raw[split + 4..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_http_reads_status_and_body() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"a\":1}";
        let (status, body) = split_http(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"{\"a\":1}");
    }

    #[test]
    fn split_http_rejects_a_headless_response() {
        assert!(split_http(b"garbage").is_err());
    }

    #[test]
    fn split_http_carries_a_daemon_refusal_status() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n{\"error\":\"invalid code\"}";
        let (status, body) = split_http(raw).unwrap();
        assert_eq!(status, 401);
        assert!(body.starts_with(b"{\"error\""));
    }

    /// A multi-certificate chain has no single SAS for the operator to compare,
    /// so it is refused rather than fingerprinting whichever came first.
    #[test]
    fn single_ca_der_requires_exactly_one_certificate() {
        let (ca_cert_pem, _ca_key_pem) = zeroclaw_tls::testing::gen_ca();
        let one = single_ca_der(&ca_cert_pem);
        assert!(one.is_ok(), "a single certificate must parse");
        let doubled = format!("{ca_cert_pem}{ca_cert_pem}");
        assert!(
            single_ca_der(&doubled).is_err(),
            "two certs must be refused"
        );
        assert!(single_ca_der("").is_err(), "an empty chain must be refused");
    }

    /// The byte cap is what stops a hostile or broken daemon growing relay
    /// memory without limit while nothing has been trusted yet.
    #[tokio::test]
    async fn read_bounded_refuses_a_response_past_the_byte_cap() {
        let (mut far, mut near) = tokio::io::duplex(8192);
        let flood = tokio::spawn(async move {
            let chunk = vec![b'x'; 8192];
            // Write past the cap; the read side gives up before this finishes.
            for _ in 0..((MAX_ENROLL_RESPONSE_BYTES / 8192) + 4) {
                if far.write_all(&chunk).await.is_err() {
                    return;
                }
            }
        });
        let error = read_bounded(&mut near)
            .await
            .expect_err("an oversized response must be refused");
        assert!(
            format!("{error:#}").contains(&MAX_ENROLL_RESPONSE_BYTES.to_string()),
            "the refusal must name the cap: {error:#}"
        );
        flood.abort();
    }

    /// A daemon that accepts the connection and then stops sending must not pin
    /// the relay task and the route it holds.
    #[tokio::test(start_paused = true)]
    async fn read_bounded_gives_up_on_a_stalled_response() {
        // Both ends held: the stream never yields data and never reaches EOF,
        // which is exactly the stall the deadline exists for. The paused clock
        // makes the wall-clock budget assert instantly.
        let (_far, mut near) = tokio::io::duplex(8192);
        let started = tokio::time::Instant::now();
        let error = read_bounded(&mut near)
            .await
            .expect_err("a stalled response must time out");
        assert!(format!("{error:#}").contains("timed out"), "got: {error:#}");
        // Assert the BUDGET, not merely that some timeout exists. Under a paused
        // clock tokio auto-advances to whatever deadline is pending, so a test
        // that only checked "it eventually failed" would pass just as happily
        // with a thousand-fold budget - it would be decorative. Pinning the
        // elapsed virtual time is what makes this sensitive to the constant.
        assert_eq!(
            started.elapsed(),
            std::time::Duration::from_secs(ENROLL_READ_TIMEOUT_SECS),
            "the stall must be cut at the documented read budget"
        );
    }

    #[test]
    fn node_ids_are_validated_before_the_registry() {
        assert!(check_node_id("node-1").is_ok());
        assert!(check_node_id("").is_err());
        assert!(check_node_id("has space").is_err());
        assert!(check_node_id(&"x".repeat(MAX_NODE_ID_LEN + 1)).is_err());
    }
}
