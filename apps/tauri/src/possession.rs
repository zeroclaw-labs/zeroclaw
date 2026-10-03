//! Proof that the dashboard's address is served by the launched core's own
//! gateway, checked on the very HTTP connection a credential then travels on.
//!
//! Over its verified RPC connection the app asks the core for a challenge for
//! the dashboard address: a fresh nonce and the proof the gateway listener
//! the core vouches for at that address answers it with. That listener is
//! the core's own gateway, or a separate `zeroclaw-gw` that registered its
//! listener with the core over its own authenticated connection; the core
//! vouches only while it accepts connections there, and the listener answers
//! only then too. The app then opens one HTTP/1.1 connection to the
//! dashboard address, sends `GET /health?challenge=<nonce>` and compares the
//! proof. Only the process holding that listener's key can produce it,
//! whatever process ID or body another one copies, and no process ID decides
//! anything. A credential is then sent on that same connection, so it
//! reaches the process that proved itself, not whichever process holds the
//! port a moment later.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::client::conn::http1::{Connection, SendRequest};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_rpc_client::{ClientError, Method, RpcClient, error_codes};

/// The `/health` query parameter carrying the nonce, and the body field the
/// core's gateway answers with.
const CHALLENGE_QUERY: &str = "challenge";
const PROOF_FIELD: &str = "challenge_proof";
/// Ceiling for connecting to the dashboard address and for each exchange.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest response body read from the dashboard address.
const MAX_BODY_BYTES: usize = 1 << 20;

/// Why the dashboard address was not proven to be the core's own gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofFailure {
    /// The core did not answer over RPC.
    CoreUnavailable(String),
    /// The core answers but predates the method named: it cannot vouch for
    /// any gateway, so nothing it serves is trusted with a credential.
    Unsupported(&'static str),
    /// No listener of the core accepts connections right now.
    NotBound,
    /// The core's listener holds another address than the dashboard's.
    ElsewhereBound(String),
    /// Nothing usable answered on the dashboard address.
    Unreachable(String),
    /// The dashboard address answered without the core's proof. `reported_pid`
    /// is the process ID it claimed, if any: a diagnostic only.
    Mismatch { reported_pid: Option<u64> },
}

impl std::fmt::Display for ProofFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CoreUnavailable(error) => write!(f, "the ZeroClaw core did not answer: {error}"),
            Self::Unsupported(method) => write!(
                f,
                "the ZeroClaw core does not support {method}; install a ZeroClaw core as new as this app"
            ),
            Self::NotBound => {
                f.write_str("the ZeroClaw core's gateway is not accepting connections")
            }
            Self::ElsewhereBound(bound) => {
                write!(f, "the ZeroClaw core's gateway is listening on {bound}")
            }
            Self::Unreachable(error) => write!(f, "the dashboard address did not answer: {error}"),
            Self::Mismatch {
                reported_pid: Some(pid),
            } => write!(
                f,
                "the dashboard address answers as process {pid} without the ZeroClaw core's proof"
            ),
            Self::Mismatch { reported_pid: None } => {
                f.write_str("the dashboard address answers without the ZeroClaw core's proof")
            }
        }
    }
}

/// A challenge the core issued for its gateway.
struct Challenge {
    nonce: String,
    proof: String,
}

/// Ask the core for a challenge for the listener it vouches for at
/// `dashboard`.
async fn challenge(core: &RpcClient, dashboard: SocketAddr) -> Result<Challenge, ProofFailure> {
    let issued = core
        .request(
            Method::GatewayPossessionChallenge,
            serde_json::json!({ "addr": dashboard.to_string() }),
        )
        .await
        .map_err(|error| refused(Method::GatewayPossessionChallenge, error))?;
    let Some(bound) = issued["bound_addr"].as_str() else {
        return Err(ProofFailure::NotBound);
    };
    if bound.parse::<SocketAddr>().ok() != Some(dashboard) {
        return Err(ProofFailure::ElsewhereBound(bound.to_string()));
    }
    match (issued["nonce"].as_str(), issued["proof"].as_str()) {
        (Some(nonce), Some(proof)) => Ok(Challenge {
            nonce: nonce.to_string(),
            proof: proof.to_string(),
        }),
        _ => Err(ProofFailure::CoreUnavailable(
            "the core issued a challenge without a nonce or proof".to_string(),
        )),
    }
}

/// Classify a refused request by its JSON-RPC code: a core that predates the
/// method is a version skew, not an outage.
fn refused(method: Method, error: ClientError) -> ProofFailure {
    match error {
        ClientError::Rpc(rpc) if rpc.code == error_codes::METHOD_NOT_FOUND => {
            ProofFailure::Unsupported(method.wire_name())
        }
        other => ProofFailure::CoreUnavailable(other.to_string()),
    }
}

/// An HTTP/1.1 connection to the dashboard address whose peer proved it
/// holds the core's possession key. Requests sent on it reach that peer. The
/// connection is driven only while a request on it is in flight, and closes
/// when this is dropped.
pub struct ProvenConnection {
    sender: SendRequest<Full<Bytes>>,
    connection: std::pin::Pin<Box<Connection<TokioIo<tokio::net::TcpStream>, Full<Bytes>>>>,
    closed: bool,
    authority: String,
    /// The `/health` the proof was answered with.
    health: Value,
}

impl ProvenConnection {
    /// The proven listener's own `/health` answer, the one that carried the
    /// proof.
    pub fn health(&self) -> &Value {
        &self.health
    }

    /// Send one request on this connection: the status and the body.
    pub async fn send(
        &mut self,
        method: hyper::Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Bytes), String> {
        let mut request = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, &self.authority);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if body.is_some() {
            request = request.header(hyper::header::CONTENT_TYPE, "application/json");
        }
        let request = request
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .map_err(|error| error.to_string())?;
        let Self {
            sender,
            connection,
            closed,
            ..
        } = self;
        let exchange = async {
            sender
                .ready()
                .await
                .map_err(|error| format!("the connection closed: {error}"))?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = Limited::new(response.into_body(), MAX_BODY_BYTES)
                .collect()
                .await
                .map_err(|error| error.to_string())?
                .to_bytes();
            Ok((status, body))
        };
        let driven = async {
            tokio::pin!(exchange);
            tokio::select! {
                biased;
                result = &mut exchange => result,
                // The peer closed the connection; what it already sent is
                // still delivered to the exchange.
                _ = connection.as_mut(), if !*closed => {
                    *closed = true;
                    (&mut exchange).await
                }
            }
        };
        tokio::time::timeout(HTTP_TIMEOUT, driven)
            .await
            .map_err(|_| format!("no answer within {} seconds", HTTP_TIMEOUT.as_secs()))?
    }
}

/// Prove that `dashboard` is the launched core's own gateway, and keep the
/// connection the proof was made on.
pub async fn prove_gateway(
    core: &RpcClient,
    dashboard: SocketAddr,
) -> Result<ProvenConnection, ProofFailure> {
    let challenge = challenge(core, dashboard).await?;
    let stream = tokio::time::timeout(HTTP_TIMEOUT, tokio::net::TcpStream::connect(dashboard))
        .await
        .map_err(|_| ProofFailure::Unreachable("connecting timed out".to_string()))?
        .map_err(|error| ProofFailure::Unreachable(error.to_string()))?;
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|error| ProofFailure::Unreachable(error.to_string()))?;
    let mut proven = ProvenConnection {
        sender,
        connection: Box::pin(connection),
        closed: false,
        authority: dashboard.to_string(),
        health: Value::Null,
    };
    let (status, body) = proven
        .send(
            hyper::Method::GET,
            &format!("/health?{CHALLENGE_QUERY}={}", challenge.nonce),
            &[],
            None,
        )
        .await
        .map_err(ProofFailure::Unreachable)?;
    let report: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let answered = report[PROOF_FIELD].as_str().unwrap_or_default();
    if status == 200 && same_proof(answered, &challenge.proof) {
        proven.health = report;
        Ok(proven)
    } else {
        Err(ProofFailure::Mismatch {
            reported_pid: report["runtime"]["pid"].as_u64(),
        })
    }
}

/// Compare two proofs without stopping at the first differing byte.
fn same_proof(answered: &str, expected: &str) -> bool {
    answered.len() == expected.len()
        && !expected.is_empty()
        && answered
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |differs, (a, b)| differs | (a ^ b))
            == 0
}

/// The launched core, for proofs and for credentials that never cross HTTP.
/// Its RPC connection is dialed again, with the endpoint's operating-system
/// account checked again, if it ends.
pub struct CoreLink {
    endpoint: PathBuf,
    bundled: bool,
    dashboard: SocketAddr,
    client: tokio::sync::Mutex<Option<Arc<RpcClient>>>,
}

impl std::fmt::Debug for CoreLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreLink")
            .field("endpoint", &self.endpoint)
            .field("dashboard", &self.dashboard)
            .finish_non_exhaustive()
    }
}

impl CoreLink {
    pub fn new(client: RpcClient, endpoint: PathBuf, bundled: bool, dashboard: SocketAddr) -> Self {
        Self {
            endpoint,
            bundled,
            dashboard,
            client: tokio::sync::Mutex::new(Some(Arc::new(client))),
        }
    }

    /// The dashboard address this core's gateway must hold.
    pub fn dashboard(&self) -> SocketAddr {
        self.dashboard
    }

    /// The core's RPC connection, dialed again if it ended.
    async fn core(&self) -> Result<Arc<RpcClient>, ProofFailure> {
        let mut client = self.client.lock().await;
        if let Some(current) = client.as_ref()
            && current.state() == zeroclaw_rpc_client::ConnectionState::Connected
        {
            return Ok(Arc::clone(current));
        }
        let fresh = Arc::new(
            crate::readiness::verify_core(&self.endpoint, None, self.bundled)
                .await
                .map_err(|failure| ProofFailure::CoreUnavailable(failure.message().to_string()))?,
        );
        *client = Some(Arc::clone(&fresh));
        Ok(fresh)
    }

    /// A connection to the dashboard address proven to be this core's
    /// gateway, made now: call it immediately before every credential is
    /// sent, and send the credential on it.
    pub async fn prove(&self) -> Result<ProvenConnection, ProofFailure> {
        let core = self.core().await?;
        prove_gateway(&core, self.dashboard).await
    }

    /// A one-time pairing code from the core, over its verified socket. No
    /// admin token is presented, and nothing crosses the dashboard's port.
    pub async fn new_pairing_code(&self) -> Result<String, String> {
        let core = self.core().await.map_err(|failure| failure.to_string())?;
        let issued = core
            .request(
                Method::PairingNewCode,
                Value::Object(serde_json::Map::new()),
            )
            .await
            .map_err(|error| match refused(Method::PairingNewCode, error) {
                unsupported @ ProofFailure::Unsupported(_) => unsupported.to_string(),
                other => format!("the core refused a pairing code: {other}"),
            })?;
        issued["pairing_code"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the core issued no pairing code".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proof_must_match_in_full() {
        assert!(same_proof("abcd", "abcd"));
        assert!(!same_proof("abce", "abcd"));
        assert!(!same_proof("abc", "abcd"));
        assert!(
            !same_proof("", ""),
            "an empty expected proof proves nothing"
        );
    }
}
