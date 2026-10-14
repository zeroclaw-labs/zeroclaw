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
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};

use crate::security::cert_ledger::{CertLedger, CertStatus, IssuanceActor, LedgerEntry};

mod paircode_admin;
pub use paircode_admin::{GeneratedEnrollmentPaircode, request_new_paircode};

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
    /// SHA-256 fingerprint of the issued certificate, carried out of
    /// [`EnrollServer::process`] so the connection handler can mark the ledger
    /// row delivered once the response write actually succeeds.
    ///
    /// `serde(skip)`: this is plumbing between two daemon-side steps, not part
    /// of the wire contract - the client already holds the certificate these
    /// bytes fingerprint.
    #[serde(skip)]
    fingerprint: String,
}

/// The preflight response body (`GET /enroll/ca`). It intentionally contains only
/// the daemon CA needed for SAS confirmation; it does not consume or receive a
/// pairing code.
#[derive(Debug, Serialize)]
struct EnrollTrustResponse {
    ca_chain_pem: String,
}

/// Everything the enrollment endpoint needs to serve requests.
/// Source ports the in-process relay bridge is dialing the enrollment endpoint
/// from. The bridge registers each outbound port BEFORE connecting (bind, then
/// register, then connect), so accept-side membership is race-free. A loopback
/// peer in this set is relay-class; every other peer is direct-class.
pub type BridgePortSet = std::sync::Arc<std::sync::Mutex<std::collections::HashSet<u16>>>;

/// Class-wide attempt budget for relay-routed enrollment. Relay clients share
/// one network identity (the bridge's loopback), so per-client lockout cannot
/// apply; this refilling bucket bounds their SUM of pairing attempts instead.
/// Throttle, not lockout: a hostile client can slow relay enrollment, never
/// freeze it for everyone. Brute-force exposure stays small because codes are
/// one-time and short-lived, and the relay's own per-node connect bucket caps
/// attempt rate upstream. When pluggable inbound authentication lands at this
/// enrollment boundary, authenticated enrollees get per-subject limits and this
/// class bucket remains only for bare pairing-code enrollment.
pub struct RelayAttemptBucket {
    state: std::sync::Mutex<(f64, std::time::Instant)>,
    burst: f64,
    rate_per_sec: f64,
}

impl RelayAttemptBucket {
    pub fn new(burst: u32, rate_per_sec: f64) -> Self {
        Self {
            state: std::sync::Mutex::new((f64::from(burst), std::time::Instant::now())),
            burst: f64::from(burst),
            rate_per_sec,
        }
    }

    /// Take one attempt token; `false` = over budget, caller answers 429.
    pub fn try_take(&self) -> bool {
        let mut st = self.state.lock().expect("relay attempt bucket lock");
        let now = std::time::Instant::now();
        let (ref mut tokens, ref mut last) = *st;
        *tokens =
            (*tokens + now.duration_since(*last).as_secs_f64() * self.rate_per_sec).min(self.burst);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Default for RelayAttemptBucket {
    /// Burst 5, refill 0.5/s: a legitimate enrollee is untouched, while a
    /// sustained brute force across ALL relay clients is held to ~30/min.
    fn default() -> Self {
        Self::new(5, 0.5)
    }
}

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
    /// Resolve the pairing-code policy from the canonical live config at the
    /// instant an operator asks the admin loop to mint another code.
    pub pairing_code_policy: Arc<dyn Fn() -> PairingCodePolicy + Send + Sync>,
    /// Static WSS pin allowlists cannot admit freshly issued in-band certs unless
    /// the operator updates the canonical pin source out of band.
    pub static_client_pins_configured: bool,
    /// RFC3339 instant before which a code-less ("unpaired") enrollment is
    /// accepted - the time-boxed migration window. `None` means closed.
    pub allow_unpaired_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Relay coordinates to hand back, when the relay bridge is configured.
    pub relay_profile: RelayProfile,
    /// Data dir used for local operator requests to mint additional pairing
    /// codes. This is intentionally not exposed through the public enrollment
    /// HTTP route because relay traffic reaches this listener over loopback.
    /// Ports the relay bridge dials enrollment from (None = no relay bridge).
    pub bridge_ports: Option<BridgePortSet>,
    /// Class-wide attempt budget for relay-routed enrollment.
    pub relay_attempt_bucket: RelayAttemptBucket,
    pub paircode_admin_data_dir: Option<PathBuf>,
}

impl EnrollServer {
    /// Authenticate, sign, record, and build the response. Returns `(status,
    /// json_error)` on any failure so the caller writes a clean HTTP error.
    async fn process(
        &self,
        req: &EnrollRequest,
        peer: &str,
        class: PeerClass,
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
        let pairing = match class {
            // Direct clients have a real network identity: per-client
            // brute-force lockout applies as before.
            PeerClass::Direct => match self.pairing.reserve_pair(pairing_code, peer).await {
                Ok(Some(pairing)) => pairing,
                Ok(None) => {
                    return Err((401, "invalid or already-used pairing code".to_string()));
                }
                Err(secs) => {
                    return Err((429, format!("too many attempts; retry in {secs}s")));
                }
            },
            // Relay-routed clients all share the bridge's loopback identity, so
            // per-peer lockout would be shared-fate: one hostile client's five
            // failures would freeze enrollment for every relay client. Bound
            // their SUM with the class bucket instead, and skip lockout
            // accounting (see PairingGuard::reserve_pair_unkeyed).
            PeerClass::RelayBridge => {
                if !self.relay_attempt_bucket.try_take() {
                    return Err((
                        429,
                        "relay enrollment attempt budget exhausted; retry shortly".to_string(),
                    ));
                }
                match self.pairing.reserve_pair_unkeyed(pairing_code).await {
                    Some(pairing) => pairing,
                    None => {
                        return Err((401, "invalid or already-used pairing code".to_string()));
                    }
                }
            }
        };
        let token_hash = pairing.token_hash();

        // 2. The daemon assigns the device identity (never the client/CSR): a
        //    stable, unguessable id that becomes the cert CN and the ledger key.
        let device_id = mint_device_id().map_err(|e| (500u16, e))?;

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

        // The row is active from here, but the client does not hold the
        // certificate yet - `deliver_enroll_response` marks it delivered only
        // once the response write succeeds. Until then the ledger treats it as
        // an undelivered credential and will revoke it (see
        // `CertLedger::record_issued`).
        Ok(EnrollResponse {
            cert_pem: issued.cert_pem,
            ca_chain_pem: self.ca_cert_pem.clone(),
            device_id,
            not_after: issued.not_after,
            relay_profile: self.relay_profile.clone(),
            fingerprint: issued.fingerprint,
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
    if let Some(data_dir) = server.paircode_admin_data_dir.clone() {
        paircode_admin::spawn_request_loop(server.clone(), data_dir, cancel.clone());
    }
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
                // Relay-class detection: the in-process bridge registered its
                // outbound source port before connecting, so membership here is
                // authoritative. Loopback-only: a remote host can never claim it.
                let class = if peer.ip().is_loopback()
                    && server
                        .bridge_ports
                        .as_ref()
                        .is_some_and(|set| {
                            set.lock().expect("bridge port set lock").contains(&peer.port())
                        }) {
                    PeerClass::RelayBridge
                } else {
                    PeerClass::Direct
                };
                zeroclaw_spawn::spawn!(async move {
                    let _permit = permit;
                    // This server holds ONE ledger for the daemon's lifetime,
                    // so it never gets an open-time sweep after startup. Any
                    // enrollment activity - including a connection that goes on
                    // to fail authentication - is a fine moment to reconcile
                    // certificates an earlier failed response left undelivered.
                    // One indexed query against a table with a row per issued
                    // certificate; the sweep is age-gated, so it cannot touch
                    // an issuance in flight.
                    sweep_undelivered(&server);
                    let peer_ip = peer.ip().to_string();
                    let fut = handle_conn(&server, tcp, &peer_ip, class);
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

/// Reconcile certificates an earlier enrollment left undelivered, logging
/// rather than failing.
///
/// Never propagates: this is maintenance for PAST issuances, and refusing to
/// serve the connection in front of us because an unrelated stale row could not
/// be revoked would trade a bounded residue for an outage. The rows stay
/// eligible for the next connection, the next issuance, or the next restart.
fn sweep_undelivered(server: &EnrollServer) {
    if let Err(error) = server.ledger.sweep_undelivered_certificates() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({ "error": format!("{error:#}") })),
            "enrollment: could not sweep undelivered certificates; they stay eligible for the \
             next enrollment, issuance, or ledger open"
        );
    }
}

/// Which trust class the accepted peer belongs to (see [`BridgePortSet`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerClass {
    /// A directly-connecting client with a real network identity.
    Direct,
    /// A logical connection tunnelled by this daemon's own relay bridge; all
    /// such peers share the bridge's loopback identity.
    RelayBridge,
}

async fn handle_conn(
    server: &EnrollServer,
    tcp: tokio::net::TcpStream,
    peer: &str,
    class: PeerClass,
) {
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
    let outcome = server.process(&req, peer, class).await;
    deliver_enroll_response(server, &mut tls, outcome).await;
}

/// Write the enrollment outcome to `stream` and, for a success, mark the ledger
/// row delivered - but ONLY if the response write actually succeeded.
///
/// This is the enrollment side of the delivery boundary. `process` has already
/// published an active ledger row for a certificate the client does not hold
/// yet; a write failure or a client that disconnected mid-response leaves that
/// row unmarked, and the ledger's undelivered sweep revokes it. Marking here
/// rather than in `process` is the whole point - `process` cannot know whether
/// the bytes went out.
///
/// Generic over the stream so the failure branch is reachable in a test with a
/// writer that errors; production passes the live TLS stream.
///
/// What "delivered" can honestly mean at this layer: `write_json` returned Ok,
/// so the response was written and flushed into the TLS session. That is not
/// proof the client parsed it - no HTTP response can be - but it is the last
/// point the daemon has any evidence about, and it is exactly the boundary the
/// undelivered sweep needs to distinguish "the client got its certificate" from
/// "the connection died holding it".
async fn deliver_enroll_response<S: AsyncWrite + Unpin>(
    server: &EnrollServer,
    stream: &mut S,
    outcome: Result<EnrollResponse, (u16, String)>,
) {
    match outcome {
        Ok(resp) => {
            let fingerprint = resp.fingerprint.clone();
            let json = serde_json::to_vec(&resp).unwrap_or_default();
            match write_json(stream, 200, "OK", &json).await {
                Ok(()) => mark_enrollment_delivered(server, &fingerprint),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "error": format!("{e}"),
                                "fingerprint": fingerprint,
                            })),
                        "enrollment response never reached the client; the issued certificate \
                         stays undelivered and will be reconciled"
                    );
                }
            }
        }
        Err((status, msg)) => {
            let reason = http_reason(status);
            let body = serde_json::to_vec(&serde_json::json!({ "error": msg })).unwrap_or_default();
            let _ = write_json(stream, status, reason, &body).await;
        }
    }
}

/// Record a delivered enrollment, logging rather than failing.
///
/// The response has already gone out by the time this runs, so there is nothing
/// left to fail: the client holds the certificate either way. A ledger that
/// cannot record the delivery therefore leaves the row undelivered and the
/// sweep revokes a certificate that DID arrive - the fail-closed direction, and
/// recoverable by re-enrolling. The alternative (assume delivery) would leave a
/// credential the ledger cannot account for, which is the failure this whole
/// protocol exists to prevent.
fn mark_enrollment_delivered(server: &EnrollServer, fingerprint: &str) {
    match server.ledger.mark_delivered(fingerprint) {
        Ok(true) => {}
        Ok(false) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "fingerprint": fingerprint })),
                "enrollment delivered but no active undelivered ledger row matched; it will be \
                 reconciled unless it was already marked"
            );
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": format!("{e}"),
                        "fingerprint": fingerprint,
                    })),
                "enrollment delivered but the ledger could not record delivery; the certificate \
                 will be reconciled and the client must re-enroll"
            );
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
/// Mint the daemon-assigned device identity.
///
/// This id becomes the certificate CN and the ledger key, so it has to be
/// unguessable. There is no acceptable weaker substitute when the system
/// CSPRNG fails: the previous fallback seeded the id from the current Unix
/// SECOND, which is predictable and collides across devices enrolling in the
/// same second. Security-path entropy failure aborts enrollment instead.
fn mint_device_id() -> Result<String, String> {
    mint_device_id_with(|buf| {
        use ring::rand::SecureRandom;
        // SystemRandom is the same CSPRNG the relay node-id path uses.
        ring::rand::SystemRandom::new().fill(buf).map_err(|_| ())
    })
}

/// [`mint_device_id`] with the entropy source injected, so the failure path is
/// testable without breaking the system CSPRNG.
fn mint_device_id_with(fill: impl FnOnce(&mut [u8]) -> Result<(), ()>) -> Result<String, String> {
    let mut bytes = [0u8; 8];
    fill(&mut bytes).map_err(|()| {
        "system CSPRNG unavailable; refusing to mint a predictable device identity".to_string()
    })?;
    Ok(format!("dev_{}", hex::encode(bytes)))
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
        let id = mint_device_id().expect("system CSPRNG must be available in tests");
        assert!(id.starts_with("dev_"));
        assert_eq!(id.len(), 4 + 16);
        assert!(id[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(mint_device_id().unwrap(), mint_device_id().unwrap());
    }

    /// Security-path entropy failure must ABORT enrollment, not silently
    /// downgrade to a predictable identity.
    #[test]
    fn mint_device_id_fails_closed_without_entropy() {
        let err =
            mint_device_id_with(|_| Err(())).expect_err("entropy failure must not mint an id");
        assert!(
            err.contains("CSPRNG"),
            "the error must name the entropy failure, got: {err}"
        );
    }

    /// The happy path still derives the id from the supplied bytes.
    #[test]
    fn mint_device_id_uses_the_supplied_entropy() {
        let id = mint_device_id_with(|buf| {
            buf.copy_from_slice(&[0xAB; 8]);
            Ok(())
        })
        .expect("a working entropy source must mint an id");
        assert_eq!(id, "dev_abababababababab");
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
            pairing_code_policy: Arc::new(PairingCodePolicy::default),
            static_client_pins_configured: false,
            allow_unpaired_until: deadline,
            relay_profile: RelayProfile::default(),
            bridge_ports: None,
            relay_attempt_bucket: RelayAttemptBucket::default(),
            paircode_admin_data_dir: None,
        }
    }

    #[tokio::test]
    async fn relay_class_bad_codes_do_not_lock_out_other_relay_clients() {
        // Regression: relay-routed clients all reach the daemon from the
        // bridge's loopback identity. Keyed lockout would let ONE hostile relay
        // client's five wrong codes freeze enrollment for EVERY relay client for
        // 300s. RelayBridge peers must use the class bucket (throttle), never
        // per-peer lockout.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let good = pairing.pairing_code().unwrap();
        let server = test_server(pairing, None);

        // A hostile relay client submits many wrong codes. All share the bridge
        // peer; none must trip a per-peer lockout that would return 5xx-flavoured
        // "retry in Ns" and freeze the class.
        for _ in 0..12 {
            let (csr, _k) = zeroclaw_tls::testing::gen_client_csr("x");
            let req = EnrollRequest {
                pairing_code: "000000".into(),
                csr_pem: csr,
            };
            let err = server
                .process(&req, "127.0.0.1", PeerClass::RelayBridge)
                .await
                .unwrap_err();
            // 401 (bad code) or 429 (budget) are both fine; a lockout would also
            // be 429 but the point is the GOOD code below must still succeed.
            assert!(matches!(err.0, 401 | 429), "unexpected status {}", err.0);
        }

        // Time passes so the refilling class bucket has a token for the honest
        // client (burst 5, refill 0.5/s).
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // A DIFFERENT relay client with the CORRECT code still enrolls. Under
        // per-peer lockout it would be frozen out by the attacker's failures.
        let (csr, _k) = zeroclaw_tls::testing::gen_client_csr("honest");
        let req = EnrollRequest {
            pairing_code: good,
            csr_pem: csr,
        };
        let resp = server
            .process(&req, "127.0.0.1", PeerClass::RelayBridge)
            .await
            .expect("an honest relay client must still enroll despite another's failures");
        assert!(resp.cert_pem.contains("BEGIN CERTIFICATE"));
    }

    #[tokio::test]
    async fn process_issues_cert_with_valid_code_and_ignores_csr_identity() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let server = test_server(pairing, None);
        // A CSR requesting an attacker CN; the daemon must mint its own device id.
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("attacker-wants-this");
        let req = EnrollRequest {
            pairing_code: code,
            csr_pem: csr,
        };
        let resp = server
            .process(&req, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap();
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
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let server = test_server_with_ledger(pairing, None, ledger);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: code,
            csr_pem: csr,
        };

        let err = server
            .process(&req, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap_err();
        assert_eq!(err.0, 500);
        assert!(err.1.contains("certificate audit event"), "got: {}", err.1);
        assert!(
            server.pairing.pairing_code().is_some(),
            "ledger/audit failure must not consume the one-time pairing code"
        );
        // The audit event is written BEFORE the ledger row, so a failed audit
        // leaves no active certificate behind. Committing the row first left an
        // orphan, and because the retry carries a fresh CSR (a different
        // fingerprint) it would multiply active credentials rather than replace
        // the first.
        assert_eq!(
            server.ledger.list_active().unwrap().len(),
            0,
            "an audit failure must not strand an active ledger row"
        );
    }

    #[tokio::test]
    async fn process_delivers_no_certificate_when_the_ledger_write_fails() {
        // The audit event is written BEFORE the ledger row, so a forced SQLite
        // failure lands after it. The append-only record must then read as an
        // attempt - not a completed issuance - and the client must get an error
        // instead of a certificate it could authenticate with.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let audit = crate::security::audit::AuditLogger::new(
            zeroclaw_config::schema::AuditConfig {
                enabled: true,
                log_path: "audit.log".to_string(),
                max_size_mb: 100,
                sign_events: false,
            },
            dir.path().to_path_buf(),
        )
        .unwrap();
        let ledger = Arc::new(CertLedger::open_in_memory(Some(Arc::new(audit))).unwrap());
        ledger.detach_issued_certs_for_test().unwrap();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let server = test_server_with_ledger(pairing, None, ledger);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: code,
            csr_pem: csr,
        };

        let err = server
            .process(&req, "1.2.3.4", PeerClass::Direct)
            .await
            .expect_err("a ledger write failure must not return a certificate");
        assert_eq!(err.0, 500);
        assert!(err.1.contains("insert issued cert"), "got: {}", err.1);
        assert!(
            server.pairing.pairing_code().is_some(),
            "a ledger failure must not consume the one-time pairing code"
        );

        let events: Vec<String> = std::fs::read_to_string(dir.path().join("audit.log"))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["event_type"]
                    .as_str()
                    .expect("event_type is a string")
                    .to_string()
            })
            .collect();
        assert_eq!(
            events,
            ["cert_issuance_attempted"],
            "the trail must record the attempt, never a completed issuance"
        );

        // The retry is the one that completes, and it issues exactly one cert.
        server.ledger.reattach_issued_certs_for_test().unwrap();
        let code = server.pairing.pairing_code().unwrap();
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let resp = server
            .process(
                &EnrollRequest {
                    pairing_code: code,
                    csr_pem: csr,
                },
                "1.2.3.4",
                PeerClass::Direct,
            )
            .await
            .unwrap();
        assert!(resp.cert_pem.contains("BEGIN CERTIFICATE"));
        assert_eq!(server.ledger.list_active().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn process_delivers_no_certificate_when_the_completion_audit_fails() {
        // The blocking case at the boundary the client actually sees. The
        // attempt event lands, the ledger row commits, and the COMPLETION event
        // fails. Before the pending -> active protocol this returned HTTP 500
        // with an ACTIVE ledger row already committed for a certificate the
        // client never received - and since the failure restores the one-time
        // pairing code, the retry (a fresh CSR, so a fresh fingerprint) turned
        // that stranded row into a SECOND active credential.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let audit = Arc::new(
            crate::security::audit::AuditLogger::new(
                zeroclaw_config::schema::AuditConfig {
                    enabled: true,
                    log_path: "audit.log".to_string(),
                    max_size_mb: 100,
                    sign_events: false,
                },
                dir.path().to_path_buf(),
            )
            .unwrap(),
        );
        let ledger = Arc::new(CertLedger::open(dir.path(), Some(audit.clone())).unwrap());
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let server = test_server_with_ledger(pairing, None, ledger);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");

        // Let the attempt write land, then fail the completion write.
        audit.fail_writes_after_for_test(1);
        let err = server
            .process(
                &EnrollRequest {
                    pairing_code: code,
                    csr_pem: csr,
                },
                "1.2.3.4",
                PeerClass::Direct,
            )
            .await
            .expect_err("a completion-audit failure must not return a certificate");
        assert_eq!(err.0, 500);
        assert!(err.1.contains("certificate audit event"), "got: {}", err.1);
        assert!(
            server.pairing.pairing_code().is_some(),
            "a completion-audit failure must not consume the one-time pairing code"
        );
        assert_eq!(
            server.ledger.list_active().unwrap().len(),
            0,
            "a certificate the client never received must hold no active ledger row"
        );

        // The retry carries a fresh CSR - a different fingerprint - exactly as
        // a real client would, and must end with ONE active credential.
        audit.clear_write_failure_for_test();
        let code = server.pairing.pairing_code().unwrap();
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let resp = server
            .process(
                &EnrollRequest {
                    pairing_code: code,
                    csr_pem: csr,
                },
                "1.2.3.4",
                PeerClass::Direct,
            )
            .await
            .unwrap();
        assert!(resp.cert_pem.contains("BEGIN CERTIFICATE"));
        let active = server.ledger.list_active().unwrap();
        assert_eq!(
            active.len(),
            1,
            "the retry must not add a second active credential"
        );
        assert_eq!(active[0].device_id, resp.device_id);

        let events: Vec<String> = std::fs::read_to_string(dir.path().join("audit.log"))
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["event_type"]
                    .as_str()
                    .expect("event_type is a string")
                    .to_string()
            })
            .collect();
        assert_eq!(
            events,
            [
                "cert_issuance_attempted",
                "cert_issuance_attempted",
                "cert_issued",
            ],
            "the interrupted issuance stays an unmatched attempt; only the retry completes"
        );
    }

    /// A stream whose every write fails: a client that vanished between the
    /// daemon signing its certificate and the response reaching it. This is the
    /// real seam - `deliver_enroll_response` takes the stream, so the failure is
    /// injected at exactly the boundary production uses, with no test hook in
    /// the production path.
    struct DisconnectedClient;

    impl AsyncWrite for DisconnectedClient {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "client disconnected",
            )))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// `delivered_at` for a fingerprint, straight from the table. The ledger
    /// deliberately exposes no reader for it, so a test asserting on delivery
    /// goes to SQLite.
    fn delivered_at(data_dir: &std::path::Path, fingerprint: &str) -> Option<i64> {
        let conn = rusqlite::Connection::open(data_dir.join("tls").join("ledger.db")).unwrap();
        conn.query_row(
            "SELECT delivered_at FROM issued_certs WHERE fingerprint = ?1",
            rusqlite::params![fingerprint],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Age a row so the ledger's undelivered sweep treats it as stale, exactly
    /// as an hour of wall clock would.
    fn backdate_issuance(data_dir: &std::path::Path, fingerprint: &str) {
        let conn = rusqlite::Connection::open(data_dir.join("tls").join("ledger.db")).unwrap();
        let changed = conn
            .execute(
                "UPDATE issued_certs SET issued_at = issued_at - 7200 WHERE fingerprint = ?1",
                rusqlite::params![fingerprint],
            )
            .unwrap();
        assert_eq!(changed, 1, "backdate fixture must hit exactly one row");
    }

    /// Enroll once against a file-backed ledger and return `(data_dir, server,
    /// response)` with the issuance recorded but NOT yet delivered.
    async fn enrolled_but_undelivered() -> (tempfile::TempDir, EnrollServer, EnrollResponse) {
        enrolled_but_undelivered_with_crl(|dir| {
            crate::security::cert_ledger::revoked_list_path(dir)
        })
        .await
    }

    /// [`enrolled_but_undelivered`] with the revocation file the ledger
    /// materializes to chosen by the caller, so a test can prove the
    /// enrollment path honours a configured `[wss.client_auth].crl_path`.
    async fn enrolled_but_undelivered_with_crl(
        crl_for: impl Fn(&std::path::Path) -> std::path::PathBuf,
    ) -> (tempfile::TempDir, EnrollServer, EnrollResponse) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(CertLedger::open_at(dir.path(), None, crl_for(dir.path())).unwrap());
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let server = test_server_with_ledger(pairing, None, ledger);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let resp = server
            .process(
                &EnrollRequest {
                    pairing_code: code,
                    csr_pem: csr,
                },
                "1.2.3.4",
                PeerClass::Direct,
            )
            .await
            .expect("enrollment must issue a certificate");
        (dir, server, resp)
    }

    #[tokio::test]
    async fn an_enrollment_response_that_never_reaches_the_client_is_reconciled_away() {
        // The reviewer's blocking case, at the boundary that actually decides
        // it. `process` has already promoted the row to ACTIVE - deliberately,
        // because the inverse ordering can hand out a certificate the ledger
        // never records - so the client disconnecting here leaves an active row
        // for a certificate nobody holds. Delivery tracking is what bounds that.
        let (dir, server, resp) = enrolled_but_undelivered().await;
        let fingerprint = resp.fingerprint.clone();
        assert_eq!(
            server.ledger.status_of(&fingerprint).unwrap(),
            Some(CertStatus::Active),
            "the row is published before the response is written - by design"
        );

        deliver_enroll_response(&server, &mut DisconnectedClient, Ok(resp)).await;

        // The write failed, so nothing claimed delivery.
        assert_eq!(
            delivered_at(dir.path(), &fingerprint),
            None,
            "a failed response write must not mark the certificate delivered"
        );
        assert_eq!(
            server.ledger.status_of(&fingerprint).unwrap(),
            Some(CertStatus::Active),
            "the ghost row is still active until the TTL passes"
        );

        // Once the delivery deadline passes, the next ledger open revokes it -
        // and the revocation reaches the file the WSS verifier reads, so the
        // certificate is refused at the handshake even if its bytes leaked.
        drop(server);
        backdate_issuance(dir.path(), &fingerprint);
        let reopened = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(
            reopened.status_of(&fingerprint).unwrap(),
            Some(CertStatus::Revoked),
            "an undelivered enrollment must be reconciled to revoked"
        );
        let crl = crate::security::cert_ledger::revoked_list_path(dir.path());
        let set = zeroclaw_tls::load_revoked_fingerprints(&crl).unwrap();
        assert!(
            set.contains(&fingerprint.to_ascii_lowercase()),
            "the reconciled revocation must reach the verifier's CRL file"
        );
    }

    #[tokio::test]
    async fn the_enrollment_sweep_revokes_into_the_configured_crl_not_the_default() {
        // The enrollment server holds ONE ledger, built once at daemon startup.
        // Building it on the ledger DEFAULT path while
        // `[wss.client_auth].crl_path` is configured meant every revocation it
        // performs - and the undelivered sweep runs entirely on this handle -
        // rewrote `<data_dir>/tls/revoked`, which the verifier never reads,
        // leaving the operator's real CRL untouched. Revoked in SQLite, still
        // accepted at the handshake: revocation failing open, which is the one
        // direction it may never fail.
        let custom_name = "operator-managed.crl";
        let (dir, server, first) = enrolled_but_undelivered_with_crl(|d| d.join(custom_name)).await;
        let ghost = first.fingerprint.clone();

        // The response never reaches the client, so the row stays undelivered.
        deliver_enroll_response(&server, &mut DisconnectedClient, Ok(first)).await;
        backdate_issuance(dir.path(), &ghost);

        sweep_undelivered(&server);

        // The revocation is in the file the verifier actually reads...
        let custom = dir.path().join(custom_name);
        let set = zeroclaw_tls::load_revoked_fingerprints(&custom)
            .expect("the configured CRL must exist");
        assert!(
            set.contains(&ghost.to_ascii_lowercase()),
            "the enrollment sweep must revoke into the CONFIGURED CRL"
        );
        // ...and not only in the default path nothing consults.
        let default_body =
            std::fs::read_to_string(crate::security::cert_ledger::revoked_list_path(dir.path()))
                .unwrap_or_default();
        assert!(
            !default_body.contains(&ghost),
            "revocation must not be written to the unused default path: {default_body:?}"
        );
        assert_eq!(
            server.ledger.status_of(&ghost).unwrap(),
            Some(CertStatus::Revoked)
        );
    }

    #[tokio::test]
    async fn a_later_enrollment_reconciles_an_earlier_undelivered_one_on_the_live_server() {
        // The liveness gap, at the layer that had it. The enrollment server
        // holds ONE Arc<CertLedger> for the daemon's lifetime, so when the
        // sweep only ran at ledger open, a failed enrollment response left an
        // active row and an unchanged CRL until the process restarted - the
        // stated one-hour bound did not hold on this path at all.
        //
        // Nothing here reopens or drops the ledger: the SAME server and the
        // SAME handle serve both enrollments.
        let (dir, server, first) = enrolled_but_undelivered().await;
        let ghost = first.fingerprint.clone();
        deliver_enroll_response(&server, &mut DisconnectedClient, Ok(first)).await;
        assert_eq!(
            delivered_at(dir.path(), &ghost),
            None,
            "the first client never received its certificate"
        );
        assert_eq!(
            server.ledger.status_of(&ghost).unwrap(),
            Some(CertStatus::Active),
            "and its row is live until something sweeps"
        );

        // Time passes past the delivery deadline. Nothing sweeps on its own -
        // this is the honest bound: the row waits for the next activity.
        backdate_issuance(dir.path(), &ghost);
        assert_eq!(
            server.ledger.status_of(&ghost).unwrap(),
            Some(CertStatus::Active),
            "the bound is per-activity, not a timer - state that honestly"
        );

        // The next client to reach the endpoint is enough. This is exactly what
        // the accept loop runs for every accepted connection, before it knows
        // whether the peer will even authenticate.
        sweep_undelivered(&server);

        // Revoked and enforced, with no restart, no reopen, and no unrelated
        // cert/renew - on the very Arc<CertLedger> the running server holds.
        assert_eq!(
            server.ledger.status_of(&ghost).unwrap(),
            Some(CertStatus::Revoked),
            "enrollment activity must reconcile an earlier undelivered issuance"
        );
        let crl = crate::security::cert_ledger::revoked_list_path(dir.path());
        let set = zeroclaw_tls::load_revoked_fingerprints(&crl).unwrap();
        assert!(
            set.contains(&ghost.to_ascii_lowercase()),
            "the verifier's CRL must carry it while the daemon keeps running"
        );

        // Repeat activity is a no-op, not a second revocation.
        sweep_undelivered(&server);
        assert_eq!(
            server.ledger.revoked_fingerprints().unwrap(),
            vec![ghost],
            "the sweep must be idempotent across connections"
        );
    }

    #[tokio::test]
    async fn a_delivered_enrollment_survives_the_undelivered_sweep() {
        // The happy path through the same seam: the response write succeeds, so
        // the row is marked delivered and the sweep leaves it alone forever.
        // Without this the previous test would pass just as well against a
        // ledger that revoked every certificate it ever issued.
        let (dir, server, resp) = enrolled_but_undelivered().await;
        let fingerprint = resp.fingerprint.clone();

        let mut written: Vec<u8> = Vec::new();
        deliver_enroll_response(&server, &mut written, Ok(resp)).await;

        let text = String::from_utf8_lossy(&written);
        assert!(text.starts_with("HTTP/1.1 200"), "got: {text}");
        assert!(text.contains("BEGIN CERTIFICATE"), "got: {text}");
        assert!(
            delivered_at(dir.path(), &fingerprint).is_some(),
            "a successful response write must record delivery"
        );

        drop(server);
        backdate_issuance(dir.path(), &fingerprint);
        let reopened = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(
            reopened.status_of(&fingerprint).unwrap(),
            Some(CertStatus::Active),
            "a certificate the client received must never be swept"
        );
        assert!(reopened.revoked_fingerprints().unwrap().is_empty());
    }

    #[tokio::test]
    async fn process_rejects_in_band_enrollment_when_static_pins_are_configured() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let mut server = test_server(pairing, None);
        server.static_client_pins_configured = true;
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: code.clone(),
            csr_pem: csr,
        };

        let err = server
            .process(&req, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap_err();
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
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let code = pairing.pairing_code().unwrap();
        let server = test_server(pairing, None);
        let bad = EnrollRequest {
            pairing_code: code.clone(),
            csr_pem: "not a csr".to_string(),
        };

        let err = server
            .process(&bad, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap_err();
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
            server
                .process(&good, "1.2.3.4", PeerClass::Direct)
                .await
                .is_ok(),
            "the restored code should work on retry"
        );
    }

    #[tokio::test]
    async fn process_rejects_wrong_code_and_consumes_one_time_code() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let server = test_server(pairing, None);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let bad = EnrollRequest {
            pairing_code: "000000".to_string(),
            csr_pem: csr,
        };
        let err = server
            .process(&bad, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap_err();
        assert_eq!(err.0, 401);
        assert!(server.ledger.list_active().unwrap().is_empty());
    }

    #[tokio::test]
    async fn process_requires_code_when_window_closed() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let server = test_server(pairing, None);
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: String::new(),
            csr_pem: csr,
        };
        let err = server
            .process(&req, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap_err();
        assert_eq!(err.0, 401);
    }

    #[tokio::test]
    async fn over_the_wire_enroll_issues_a_cert_through_tls() {
        // Make-or-break: a real TLS client POSTs a CSR over the server-auth
        // enrollment endpoint and gets back a 200 with a signed certificate.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
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
        let pairing = PairingGuard::new(true, &[], PairingCodePolicy::default());
        let future = chrono::Utc::now() + chrono::Duration::minutes(10);
        let server = test_server(pairing, Some(future));
        let (csr, _key) = zeroclaw_tls::testing::gen_client_csr("dev");
        let req = EnrollRequest {
            pairing_code: String::new(),
            csr_pem: csr,
        };
        let err = server
            .process(&req, "1.2.3.4", PeerClass::Direct)
            .await
            .unwrap_err();
        assert_eq!(err.0, 401);
        assert!(server.ledger.list_active().unwrap().is_empty());
    }
}
