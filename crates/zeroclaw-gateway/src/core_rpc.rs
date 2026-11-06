//! The gateway's RPC connections to the core.
//!
//! This is the strangler seam for the gateway split. A [`CoreRpc`] holds one
//! connection to the daemon's dispatcher and keeps it current across
//! reconnects. It dials one of two ways.
//!
//! In process ([`CoreRpc::attach_inproc`]): while the gateway still runs
//! inside the daemon, it dials the dispatcher through the daemon's in-process
//! connector, and routes that migrate onto RPC reach that connection through
//! the `CoreRpc` request extension. The in-process transport never takes the
//! daemon's local compatibility path: an `initialize` without an explicit
//! credential is refused with `AUTH_REQUIRED`. Until the gateway has a
//! credential of its own to present (its service key, or a forwarded user
//! bearer, both later work), the seam therefore stays idle by design. A
//! refusal is not retried, because the daemon's policy does not change until a
//! reload restarts this generation.
//!
//! Over the daemon's local socket ([`CoreRpc::attach_local`], Unix only): the
//! standalone `zeroclaw gateway` process dials the socket of the daemon
//! serving its config, as the operating-system user it runs as. Before it
//! sends anything, it requires the process serving the socket to run as its
//! own effective uid, so a socket bound by another user (through a
//! `ZEROCLAW_SOCKET` in a shared directory, say) never receives the webhooks
//! it forwards. That daemon restarts and reloads independently of the
//! gateway, and an operator can change the policy that refused it, so a
//! refused handshake or a foreign peer is retried on a slow backoff capped at
//! five minutes. Windows has no local dial: named pipes share one global
//! namespace, and nothing here can verify which process serves the daemon's
//! pipe yet.
//!
//! Transport failures retry with a short backoff on both paths. Only changes
//! of state are logged: connected, lost, unreachable, refused, and untrusted
//! peer.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, FORBIDDEN};
#[cfg(unix)]
use zeroclaw_config::schema::Config;
use zeroclaw_rpc_client::{Backoff, ClientError, ConnectOptions, RpcClient};
use zeroclaw_runtime::rpc::inproc::InprocConnector;

/// Bounds of the redial delay after a transport failure or a lost connection.
const REDIAL_INITIAL: Duration = Duration::from_millis(100);
const REDIAL_CAP: Duration = Duration::from_secs(5);
/// Bounds of the redial delay after the daemon refused the handshake, or the
/// socket was served by another user. Each refusal is an audit record in the
/// daemon, so the delay doubles up to the cap while the refusal lasts.
#[cfg(unix)]
const REFUSED_REDIAL_INITIAL: Duration = Duration::from_secs(30);
#[cfg(unix)]
const REFUSED_REDIAL_CAP: Duration = Duration::from_secs(300);

/// The state of one connection to the core, as request handlers see it.
#[derive(Clone, Default)]
pub enum CoreLink {
    /// Not attached yet, dialing, or between connections.
    #[default]
    Disconnected,
    /// An authenticated connection.
    Connected(Arc<RpcClient>),
    /// The core answered the last handshake with this JSON-RPC error code.
    Refused { code: i32 },
}

/// Handle to one core connection. Cheap to clone; clones share the
/// connection.
#[derive(Clone, Default)]
pub struct CoreRpc {
    link: Arc<RwLock<CoreLink>>,
}

impl CoreRpc {
    /// The live client, or `None` while disconnected, refused, or unattached.
    pub async fn client(&self) -> Option<Arc<RpcClient>> {
        match self.link().await {
            CoreLink::Connected(client) => Some(client),
            CoreLink::Disconnected | CoreLink::Refused { .. } => None,
        }
    }

    /// Whether a connection is currently established.
    pub async fn is_connected(&self) -> bool {
        matches!(self.link().await, CoreLink::Connected(_))
    }

    /// A snapshot of the connection state. The lock is released before this
    /// returns, so callers never hold it across their own awaits.
    pub async fn link(&self) -> CoreLink {
        self.link.read().await.clone()
    }

    /// Dial the core through `connector`, presenting `options` in every
    /// handshake, and keep the handle current across reconnects until the
    /// generation is cancelled.
    pub fn attach_inproc(&self, connector: InprocConnector, options: ConnectOptions) {
        self.attach(Dialer::Inproc(connector), options, CancellationToken::new());
    }

    /// Dial the daemon listening on `socket_path`, presenting `options` in
    /// every handshake, and keep the handle current across daemon restarts
    /// until `stop` is cancelled. Only a socket served by this process's
    /// effective uid is used. Stopping closes the connection.
    #[cfg(unix)]
    pub fn attach_local(
        &self,
        socket_path: PathBuf,
        options: ConnectOptions,
        stop: CancellationToken,
    ) {
        self.attach(
            Dialer::Local {
                socket_path,
                daemon_uid: effective_uid(),
            },
            options,
            stop,
        );
    }

    fn attach(&self, dialer: Dialer, options: ConnectOptions, stop: CancellationToken) {
        let link = Arc::clone(&self.link);
        zeroclaw_spawn::spawn!(maintain(dialer, options, link, stop));
    }

    #[cfg(test)]
    pub(crate) async fn connected_for_test(client: RpcClient) -> Self {
        let core = Self::default();
        *core.link.write().await = CoreLink::Connected(Arc::new(client));
        core
    }

    #[cfg(test)]
    pub(crate) async fn refused_for_test(code: i32) -> Self {
        let core = Self::default();
        *core.link.write().await = CoreLink::Refused { code };
        core
    }
}

/// The local socket of the daemon serving `config`: `ZEROCLAW_SOCKET` when
/// set, otherwise the default under `config.data_dir`. The daemon binds the
/// same endpoint for the same config.
#[cfg(unix)]
#[must_use]
pub fn daemon_endpoint(config: &Config) -> PathBuf {
    zeroclaw_rpc_client::endpoint::resolve_socket_path(&config.data_dir)
}

/// This process's effective uid, the only uid whose daemon the gateway
/// forwards to.
#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` is a parameter-free process query with no pointer
    // arguments or memory ownership requirements.
    unsafe { libc::geteuid() }
}

/// Whether the process serving a local socket may receive the gateway's
/// traffic: only one running as `daemon_uid`. A peer whose uid the kernel
/// did not report is refused.
#[cfg(unix)]
fn peer_is_trusted(peer_uid: Option<u32>, daemon_uid: u32) -> bool {
    peer_uid == Some(daemon_uid)
}

/// How a [`maintain`] loop reaches the core.
enum Dialer {
    Inproc(InprocConnector),
    /// The daemon's local socket, used only while it is served by
    /// `daemon_uid`.
    #[cfg(unix)]
    Local {
        socket_path: PathBuf,
        daemon_uid: u32,
    },
}

/// Why a dial produced no connection.
enum DialError {
    /// The transport or the handshake failed, or the core refused it.
    Client(ClientError),
    /// The local socket is served by another user, or by one the kernel did
    /// not report. Nothing was sent on it.
    #[cfg(unix)]
    UntrustedPeer {
        peer_uid: Option<u32>,
        daemon_uid: u32,
    },
}

impl Dialer {
    fn transport(&self) -> &'static str {
        match self {
            Self::Inproc(_) => "inproc",
            #[cfg(unix)]
            Self::Local { .. } => "local",
        }
    }

    /// Open a connection and run the handshake. `None` once there is nothing
    /// left to dial, because the in-process generation ended.
    async fn dial(&self, options: &ConnectOptions) -> Option<Result<RpcClient, DialError>> {
        match self {
            Self::Inproc(connector) => {
                let stream = connector.connect().await?;
                Some(
                    RpcClient::connect_over(stream, options.clone())
                        .await
                        .map_err(DialError::Client),
                )
            }
            #[cfg(unix)]
            Self::Local {
                socket_path,
                daemon_uid,
            } => Some(dial_local(socket_path, *daemon_uid, options).await),
        }
    }

    /// The backoff between refused dials, or `None` when a refusal ends the
    /// loop. The in-process refusal holds for the whole generation, while the
    /// daemon behind a local socket can change the policy that refused it.
    fn refusal_backoff(&self) -> Option<Backoff> {
        match self {
            Self::Inproc(_) => None,
            #[cfg(unix)]
            Self::Local { .. } => Some(Backoff::new(REFUSED_REDIAL_INITIAL, REFUSED_REDIAL_CAP)),
        }
    }
}

/// Connect to `socket_path`, check who serves it, and only then run the
/// handshake over that same stream. A foreign peer's stream is closed
/// unused.
#[cfg(unix)]
async fn dial_local(
    socket_path: &Path,
    daemon_uid: u32,
    options: &ConnectOptions,
) -> Result<RpcClient, DialError> {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|error| DialError::Client(ClientError::Io(error)))?;
    let peer_uid = stream.peer_cred().ok().map(|cred| cred.uid());
    if !peer_is_trusted(peer_uid, daemon_uid) {
        return Err(DialError::UntrustedPeer {
            peer_uid,
            daemon_uid,
        });
    }
    RpcClient::connect_over(stream, options.clone())
        .await
        .map_err(DialError::Client)
}

/// The last state a [`maintain`] loop logged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reported {
    Nothing,
    Connected,
    Lost,
    Unreachable,
    Refused(i32),
    #[cfg(unix)]
    UntrustedPeer(Option<u32>),
}

/// Log `record` only when `now` differs from what was last reported.
fn report(reported: &mut Reported, now: Reported, record: impl FnOnce()) {
    if *reported != now {
        record();
        *reported = now;
    }
}

async fn maintain(
    dialer: Dialer,
    options: ConnectOptions,
    link: Arc<RwLock<CoreLink>>,
    stop: CancellationToken,
) {
    let transport = dialer.transport();
    let mut backoff = Backoff::new(REDIAL_INITIAL, REDIAL_CAP);
    let mut refused_backoff = dialer.refusal_backoff();
    let mut reported = Reported::Nothing;
    loop {
        let dialed = tokio::select! {
            biased;
            () = stop.cancelled() => None,
            dialed = dialer.dial(&options) => dialed,
        };
        let Some(dialed) = dialed else {
            *link.write().await = CoreLink::Disconnected;
            return;
        };
        let delay = match dialed {
            Ok(client) => {
                backoff.reset();
                if let Some(refused_backoff) = refused_backoff.as_mut() {
                    refused_backoff.reset();
                }
                let client = Arc::new(client);
                *link.write().await = CoreLink::Connected(Arc::clone(&client));
                report(&mut reported, Reported::Connected, || {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "transport": transport,
                                "principal_id": client.handshake().principal_id,
                                "capabilities": client.handshake().capabilities.len(),
                            })),
                        "gateway connected to the core over RPC"
                    );
                });
                let stopped = tokio::select! {
                    biased;
                    () = stop.cancelled() => true,
                    () = client.closed() => false,
                };
                *link.write().await = CoreLink::Disconnected;
                if stopped {
                    client.shutdown();
                    return;
                }
                report(&mut reported, Reported::Lost, || {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "transport": transport,
                                "error_key": "core_rpc_lost",
                            })),
                        "gateway lost its RPC connection to the core; reconnecting"
                    );
                });
                backoff.next_delay()
            }
            Err(DialError::Client(ClientError::Rpc(error))) => {
                *link.write().await = CoreLink::Refused { code: error.code };
                let retry = refused_backoff.as_mut().map(Backoff::next_delay);
                report(&mut reported, Reported::Refused(error.code), || {
                    record_refusal(transport, &error, retry);
                });
                match retry {
                    Some(delay) => delay,
                    None => return,
                }
            }
            #[cfg(unix)]
            Err(DialError::UntrustedPeer {
                peer_uid,
                daemon_uid,
            }) => {
                *link.write().await = CoreLink::Disconnected;
                let retry = refused_backoff.as_mut().map(Backoff::next_delay);
                report(&mut reported, Reported::UntrustedPeer(peer_uid), || {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "transport": transport,
                                "peer_uid": peer_uid,
                                "expected_uid": daemon_uid,
                                "retry_in_ms": retry.map(|delay| delay.as_millis()),
                                "error_key": "core_rpc_untrusted_peer",
                            })),
                        "gateway refused a core endpoint served by another user"
                    );
                });
                match retry {
                    Some(delay) => delay,
                    None => return,
                }
            }
            Err(DialError::Client(error)) => {
                *link.write().await = CoreLink::Disconnected;
                let delay = backoff.next_delay();
                report(&mut reported, Reported::Unreachable, || {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "transport": transport,
                                "error": error.to_string(),
                                "retry_in_ms": delay.as_millis(),
                                "error_key": "core_rpc_unreachable",
                            })),
                        "gateway cannot reach the core over RPC; retrying"
                    );
                });
                delay
            }
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => {
                *link.write().await = CoreLink::Disconnected;
                return;
            }
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// Log a refused handshake. Missing or rejected credentials are operator
/// policy, so they are INFO; any other refusal (a protocol version mismatch,
/// say) is WARN with the daemon's message.
fn record_refusal(transport: &str, error: &JsonRpcError, retry: Option<Duration>) {
    let attrs = ::serde_json::json!({
        "transport": transport,
        "code": error.code,
        "message": error.message,
        "retry_in_ms": retry.map(|delay| delay.as_millis()),
        "error_key": "core_rpc_refused",
    });
    match (error.code, retry) {
        (AUTH_REQUIRED, None) => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(attrs),
            "in-process RPC seam idle: the gateway has no credential to present yet"
        ),
        (AUTH_REQUIRED | FORBIDDEN, _) => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_attrs(attrs),
            "the core refused the gateway's RPC connection"
        ),
        _ => ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(attrs),
            "the core refused the gateway's RPC handshake"
        ),
    }
}

/// A scripted core for tests: one connection, served the way the daemon's
/// dispatcher answers it.
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use zeroclaw_rpc_client::RPC_PROTOCOL_VERSION;

    /// One connection served by [`serve_fake_core`].
    pub(crate) struct FakeCore {
        /// Every frame the gateway sent, in order. Ends once the connection
        /// closes, from either side.
        pub(crate) frames: mpsc::UnboundedReceiver<Value>,
        /// Cancel to close the connection from the core's side.
        pub(crate) close: CancellationToken,
    }

    /// Serve `stream` as a core: answer `initialize` with `capabilities`, or
    /// refuse it with `init_error`, and answer every other request with
    /// `answer(method, params)`: a result, an error code, or, for `None`,
    /// nothing at all.
    pub(crate) fn serve_fake_core<S>(
        stream: S,
        capabilities: &[&str],
        init_error: Option<i32>,
        answer: impl Fn(&str, &Value) -> Option<Result<Value, i32>> + Send + 'static,
    ) -> FakeCore
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (frames_tx, frames) = mpsc::unbounded_channel();
        let close = CancellationToken::new();
        let closed = close.clone();
        let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_string()).collect();
        zeroclaw_spawn::spawn!(async move {
            let (read_half, mut write_half) = tokio::io::split(stream);
            let mut lines = BufReader::new(read_half).lines();
            loop {
                let line = tokio::select! {
                    biased;
                    () = closed.cancelled() => break,
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => line,
                        Ok(None) | Err(_) => break,
                    },
                };
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let _ = frames_tx.send(frame.clone());
                let (Some(method), Some(id)) = (frame["method"].as_str(), frame.get("id")) else {
                    continue;
                };
                let reply = if method == "initialize" {
                    Some(init_error.map_or_else(
                        || {
                            Ok(json!({
                                "protocol_version": RPC_PROTOCOL_VERSION,
                                "server_version": "test",
                                "server_pid": 7,
                                "capabilities": capabilities,
                                "principal_id": "shared-operator",
                                "commands": [],
                            }))
                        },
                        Err,
                    ))
                } else {
                    answer(method, &frame["params"])
                };
                let response = match reply {
                    None => continue,
                    Some(Ok(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Some(Err(code)) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": code, "message": "refused by the test core"},
                    }),
                };
                if write_half
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        FakeCore { frames, close }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::serve_fake_core;
    use super::*;
    use zeroclaw_rpc_client::Method;

    /// How long a test waits for something that should happen promptly.
    const PROMPT: Duration = Duration::from_secs(5);

    async fn wait_until(core: &CoreRpc, mut done: impl FnMut(&CoreLink) -> bool) {
        let deadline = tokio::time::Instant::now() + PROMPT;
        loop {
            if done(&core.link().await) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the link never reached the expected state"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn an_unattached_seam_reports_no_client() {
        let core = CoreRpc::default();
        assert!(!core.is_connected().await);
        assert!(core.client().await.is_none());
        assert!(matches!(core.link().await, CoreLink::Disconnected));
    }

    #[tokio::test]
    async fn a_connected_seam_hands_out_its_client() {
        let (client_half, core_half) = tokio::io::duplex(64 * 1024);
        let mut fake = serve_fake_core(core_half, &["status"], None, |_, _| None);
        let client = RpcClient::connect_over(client_half, ConnectOptions::default())
            .await
            .expect("the fake core completes the handshake");
        let core = CoreRpc::connected_for_test(client).await;
        assert!(core.is_connected().await);
        let client = core.client().await.expect("a connected seam has a client");
        assert!(client.supports(Method::Status));
        let handshake = fake
            .frames
            .recv()
            .await
            .expect("the core saw the handshake");
        assert_eq!(handshake["method"], "initialize");

        fake.close.cancel();
        tokio::time::timeout(Duration::from_secs(2), client.closed())
            .await
            .expect("the client observes the core closing");
    }

    #[tokio::test]
    async fn a_refused_seam_reports_its_code_and_no_client() {
        let core = CoreRpc::refused_for_test(AUTH_REQUIRED).await;
        assert!(!core.is_connected().await);
        assert!(core.client().await.is_none());
        assert!(matches!(
            core.link().await,
            CoreLink::Refused {
                code: AUTH_REQUIRED
            }
        ));
    }

    /// The in-process seam against the daemon's real dispatcher.
    mod inproc {
        use super::*;
        use zeroclaw_infra::session_queue::SessionActorQueue;
        use zeroclaw_runtime::rpc::context::RpcContext;
        use zeroclaw_runtime::rpc::session::SessionStore;

        const PAIRED_TOKEN: &str = "zc_gateway_seam_test_token";

        /// A connector bound to a live dispatcher for a config under `tmp`,
        /// with `paired_token` as its only pairing token when given. The
        /// generation ends when the returned guard drops.
        ///
        /// The duplex is served under the non-local session policy, whose
        /// `initialize` refuses every caller while TUI identity signing is
        /// off, so the config dir gets a `.secret_key`, as a daemon's has.
        /// A refusal then comes from the credential layer under test.
        fn bound_connector(
            tmp: &tempfile::TempDir,
            paired_token: Option<&str>,
        ) -> (InprocConnector, tokio_util::sync::DropGuard) {
            std::fs::write(tmp.path().join(".secret_key"), "42".repeat(32))
                .expect("write the TUI signing key");
            let mut config = zeroclaw_config::schema::Config {
                data_dir: tmp.path().join("data"),
                config_path: tmp.path().join("config.toml"),
                ..zeroclaw_config::schema::Config::default()
            };
            if let Some(token) = paired_token {
                config.gateway.require_pairing = true;
                config.gateway.paired_tokens = vec![token.to_string()];
            }
            let sessions = Arc::new(SessionStore::new(
                16,
                Arc::new(SessionActorQueue::new(4, 10, 60)),
            ));
            let generation = CancellationToken::new();
            let connector = InprocConnector::new(generation.clone());
            connector.bind(RpcContext::for_live_test(config, sessions));
            (connector, generation.drop_guard())
        }

        #[tokio::test]
        async fn a_refused_inproc_handshake_ends_the_loop_without_redialing() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let (connector, _generation) = bound_connector(&tmp, None);
            let link = Arc::new(RwLock::new(CoreLink::Disconnected));
            // The dial carries no credential, which the in-process transport
            // always refuses; the loop returning is the proof that it does
            // not dial again.
            tokio::time::timeout(
                PROMPT,
                maintain(
                    Dialer::Inproc(connector),
                    ConnectOptions::default(),
                    Arc::clone(&link),
                    CancellationToken::new(),
                ),
            )
            .await
            .expect("a refused in-process handshake ends the loop");
            assert!(matches!(
                *link.read().await,
                CoreLink::Refused {
                    code: AUTH_REQUIRED
                }
            ));
        }

        #[tokio::test]
        async fn a_lost_inproc_connection_redials_after_a_backoff() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let (connector, _generation) = bound_connector(&tmp, Some(PAIRED_TOKEN));
            let core = CoreRpc::default();
            core.attach_inproc(
                connector,
                ConnectOptions {
                    auth_token: Some(PAIRED_TOKEN.to_string()),
                    ..ConnectOptions::default()
                },
            );
            wait_until(&core, |link| matches!(link, CoreLink::Connected(_))).await;
            let first = core.client().await.expect("the seam is connected");

            let lost_at = tokio::time::Instant::now();
            first.shutdown();
            wait_until(&core, |link| match link {
                CoreLink::Connected(client) => !Arc::ptr_eq(client, &first),
                CoreLink::Disconnected | CoreLink::Refused { .. } => false,
            })
            .await;
            // The first delay is jittered to at least three quarters of
            // `REDIAL_INITIAL`; an immediate redial would take a millisecond
            // or two.
            assert!(
                lost_at.elapsed() >= REDIAL_INITIAL / 2,
                "the seam redialed after {:?}, without waiting out a backoff",
                lost_at.elapsed()
            );
        }
    }

    #[cfg(unix)]
    mod local {
        use super::*;
        use crate::core_rpc::test_support::FakeCore;
        use tokio::net::UnixListener;
        use tokio::sync::mpsc;

        #[test]
        fn the_standalone_gateway_dials_the_endpoint_the_daemon_binds() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let config = Config {
                data_dir: tmp.path().join("data"),
                config_path: tmp.path().join("config.toml"),
                ..Config::default()
            };
            // Both resolvers read `ZEROCLAW_SOCKET`, so they agree whether or
            // not the environment running this test sets it.
            assert_eq!(
                daemon_endpoint(&config),
                zeroclaw_runtime::rpc::local::socket_path(&config)
            );
        }

        #[test]
        fn only_a_peer_running_as_the_gateway_user_is_trusted() {
            assert!(peer_is_trusted(Some(501), 501));
            assert!(!peer_is_trusted(Some(502), 501));
            assert!(!peer_is_trusted(Some(0), 501), "root is another user");
            assert!(!peer_is_trusted(None, 501), "an unreported peer is refused");
        }

        /// Serve every connection accepted on `listener` as a fake core, and
        /// hand each one to the test as it arrives.
        fn accept_fake_cores(
            listener: UnixListener,
            init_error: Option<i32>,
        ) -> mpsc::UnboundedReceiver<FakeCore> {
            let (accepted_tx, accepted) = mpsc::unbounded_channel();
            zeroclaw_spawn::spawn!(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let core = serve_fake_core(stream, &["status"], init_error, |_, _| {
                        Some(Ok(serde_json::json!({})))
                    });
                    if accepted_tx.send(core).is_err() {
                        break;
                    }
                }
            });
            accepted
        }

        async fn next_accept(accepted: &mut mpsc::UnboundedReceiver<FakeCore>) -> FakeCore {
            tokio::time::timeout(PROMPT, accepted.recv())
                .await
                .expect("the gateway dials the core")
                .expect("the listener is alive")
        }

        fn socket(tmp: &tempfile::TempDir) -> PathBuf {
            tmp.path().join("daemon.sock")
        }

        #[tokio::test]
        async fn attach_local_publishes_the_client_once_the_handshake_succeeds() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let listener = UnixListener::bind(socket(&tmp)).expect("bind the fake core");
            let mut accepted = accept_fake_cores(listener, None);
            let core = CoreRpc::default();
            let stop = CancellationToken::new();
            let _stop_on_exit = stop.clone().drop_guard();
            core.attach_local(socket(&tmp), ConnectOptions::default(), stop);

            let _fake = next_accept(&mut accepted).await;
            wait_until(&core, |link| matches!(link, CoreLink::Connected(_))).await;
            let client = core.client().await.expect("the link is connected");
            assert!(client.supports(Method::Status));
            assert_eq!(
                client
                    .request(Method::Status, serde_json::json!({}))
                    .await
                    .expect("the fake core answers"),
                serde_json::json!({})
            );
        }

        #[tokio::test]
        async fn attach_local_closes_a_socket_served_by_another_user_unused() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let listener = UnixListener::bind(socket(&tmp)).expect("bind the fake core");
            let mut accepted = accept_fake_cores(listener, None);
            let core = CoreRpc::default();
            let stop = CancellationToken::new();
            let _stop_on_exit = stop.clone().drop_guard();
            // The fake core runs as this test's uid, so a gateway expecting
            // any other uid sees a foreign peer.
            core.attach(
                Dialer::Local {
                    socket_path: socket(&tmp),
                    daemon_uid: effective_uid().wrapping_add(1),
                },
                ConnectOptions::default(),
                stop,
            );

            let mut foreign = next_accept(&mut accepted).await;
            let sent = tokio::time::timeout(PROMPT, foreign.frames.recv())
                .await
                .expect("the gateway closes the foreign connection");
            assert!(
                sent.is_none(),
                "the gateway sent {sent:?} to a peer it does not trust"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(matches!(core.link().await, CoreLink::Disconnected));
            assert!(
                accepted.try_recv().is_err(),
                "a refused peer waits out the refusal backoff before the next dial"
            );
        }

        #[tokio::test]
        async fn attach_local_redials_after_the_core_closes_the_connection() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let listener = UnixListener::bind(socket(&tmp)).expect("bind the fake core");
            let mut accepted = accept_fake_cores(listener, None);
            let core = CoreRpc::default();
            let stop = CancellationToken::new();
            let _stop_on_exit = stop.clone().drop_guard();
            core.attach_local(socket(&tmp), ConnectOptions::default(), stop);

            let first = next_accept(&mut accepted).await;
            wait_until(&core, |link| matches!(link, CoreLink::Connected(_))).await;
            let first_client = core.client().await.expect("the link is connected");
            first.close.cancel();

            let _second = next_accept(&mut accepted).await;
            wait_until(&core, |link| match link {
                CoreLink::Connected(client) => !Arc::ptr_eq(client, &first_client),
                CoreLink::Disconnected | CoreLink::Refused { .. } => false,
            })
            .await;
        }

        #[tokio::test]
        async fn attach_local_waits_for_a_core_that_is_not_running_yet() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let core = CoreRpc::default();
            let stop = CancellationToken::new();
            let _stop_on_exit = stop.clone().drop_guard();
            core.attach_local(socket(&tmp), ConnectOptions::default(), stop);

            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(matches!(core.link().await, CoreLink::Disconnected));
            let listener = UnixListener::bind(socket(&tmp)).expect("bind the fake core");
            let mut accepted = accept_fake_cores(listener, None);
            let _fake = next_accept(&mut accepted).await;
            wait_until(&core, |link| matches!(link, CoreLink::Connected(_))).await;
        }

        #[tokio::test]
        async fn attach_local_records_a_refused_handshake_without_redialing_at_once() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let listener = UnixListener::bind(socket(&tmp)).expect("bind the fake core");
            let mut accepted = accept_fake_cores(listener, Some(AUTH_REQUIRED));
            let core = CoreRpc::default();
            let stop = CancellationToken::new();
            let _stop_on_exit = stop.clone().drop_guard();
            core.attach_local(socket(&tmp), ConnectOptions::default(), stop);

            let _refused = next_accept(&mut accepted).await;
            wait_until(&core, |link| {
                matches!(
                    link,
                    CoreLink::Refused {
                        code: AUTH_REQUIRED
                    }
                )
            })
            .await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(
                accepted.try_recv().is_err(),
                "a refused dial waits out its backoff before asking again"
            );
            assert!(core.client().await.is_none());
        }

        #[tokio::test]
        async fn stopping_attach_local_disconnects_and_ends_the_loop() {
            let tmp = tempfile::TempDir::new().expect("temp dir");
            let listener = UnixListener::bind(socket(&tmp)).expect("bind the fake core");
            let mut accepted = accept_fake_cores(listener, None);
            let core = CoreRpc::default();
            let stop = CancellationToken::new();
            core.attach_local(socket(&tmp), ConnectOptions::default(), stop.clone());

            let mut fake = next_accept(&mut accepted).await;
            wait_until(&core, |link| matches!(link, CoreLink::Connected(_))).await;
            stop.cancel();
            let eof = tokio::time::timeout(Duration::from_secs(2), async {
                while fake.frames.recv().await.is_some() {}
            })
            .await;
            assert!(eof.is_ok(), "stopping closes the core connection");
            wait_until(&core, |link| matches!(link, CoreLink::Disconnected)).await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert!(
                accepted.try_recv().is_err(),
                "a stopped loop never dials again"
            );
        }
    }
}
