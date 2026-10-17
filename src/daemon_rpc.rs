//! One-shot JSON-RPC client for the running daemon's local IPC endpoint.
//!
//! Most CLI commands edit config.toml directly. An edit the running daemon
//! must enforce has to go through the daemon instead, so the file and the
//! enforced state cannot disagree. This client opens one connection, checks
//! that the process at its other end is the daemon serving this
//! configuration, completes the `initialize` handshake, sends one request and
//! returns its result.
//!
//! The error variants separate the cases a caller handles differently:
//! nothing listening, a handshake that failed before any request was sent, a
//! daemon that is not this configuration's, a daemon of another release, a
//! daemon that refused this caller at the handshake and so bound no
//! principal, a daemon that bound a principal and then refused it the
//! request, a daemon that rejected the request itself, and a request that
//! was written but never answered, whose outcome is therefore unknown.
//!
//! On a Unix socket the kernel reports the account at the other end and,
//! where it records one, the process, and the client checks both before it
//! sends anything. On a Windows named pipe the kernel reports the process
//! that created the server end; the client requires it to be the one the
//! heartbeat names and to run as this account or SYSTEM, again before it
//! sends anything. The pid a server reports at `initialize` is only that
//! server's own claim, and a pipe another account created first would receive
//! the edit, which may carry a secret, so a check the kernel cannot answer
//! leaves the pipe's server unverified and the client sends nothing over it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, FORBIDDEN};
use zeroclaw_api::jsonrpc::{JsonRpcError, JsonRpcFrame, JsonRpcRequest};
use zeroclaw_runtime::rpc::dispatch::RPC_PROTOCOL_VERSION;
use zeroclaw_runtime::rpc::types::InitializeResult;

#[cfg(unix)]
pub(crate) use unix::call;
#[cfg(windows)]
pub(crate) use windows::call;

/// The release this CLI was built from. A daemon of another release may
/// read a request differently, so it is never sent one.
pub(crate) const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the daemon has to answer `initialize`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the daemon has to answer the request once it is being
/// written. A config write waits for the daemon's config write lock,
/// which another writer may hold for a while.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest frame read from the daemon, the same cap the daemon applies to
/// what it reads.
const MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;

const INITIALIZE_METHOD: &str = "initialize";
const INITIALIZE_ID: &str = "cli-initialize";
const REQUEST_ID: &str = "cli-request";

/// Why a daemon call produced no result.
#[derive(Debug)]
pub(crate) enum DaemonCallError {
    /// Nothing is listening at the endpoint (connect failed, e.g. NotFound /
    /// ConnectionRefused).
    Unavailable {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The endpoint answered but the handshake is unusable before any request
    /// was sent: a protocol error, or an `initialize` error other than a
    /// refusal.
    Handshake { detail: String },
    /// The endpoint answered, but nothing shows it is the daemon serving this
    /// configuration, so the request was not sent: the process at its other
    /// end is not the one this configuration's heartbeat records, or it runs
    /// under an account that may not serve this configuration. `detail`
    /// names the endpoint and the mismatch.
    OtherDaemon { detail: String },
    /// The handshake completed, but the daemon's `server_version` differs
    /// from this CLI's, so the request was not sent. `daemon` is its version.
    VersionMismatch { daemon: String },
    /// `initialize` answered AUTH_REQUIRED or FORBIDDEN: the daemon bound no
    /// principal to this caller, so the request was not sent.
    HandshakeRefused { code: i64, message: String },
    /// `initialize` bound a principal to this caller, and the request
    /// answered AUTH_REQUIRED or FORBIDDEN: that principal may not make it.
    RequestRefused { code: i64, message: String },
    /// The daemon accepted the caller but rejected the request (any other
    /// JSON-RPC error, e.g. INVALID_PARAMS, INTERNAL_ERROR).
    Rejected { code: i64, message: String },
    /// The request was written but no matching answer arrived (timeout or
    /// EOF): the outcome is unknown.
    NoAnswer { detail: String },
    /// A check of which process serves the daemon's named pipe could not be
    /// completed, so the call sent nothing. `detail` names the endpoint and
    /// the check.
    #[cfg(windows)]
    UnverifiableEndpoint { detail: String },
}

impl fmt::Display for DaemonCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { path, source } => {
                write!(f, "nothing is listening at {}: {source}", path.display())
            }
            Self::Handshake { detail } => write!(f, "the daemon handshake failed: {detail}"),
            Self::OtherDaemon { detail } => {
                write!(
                    f,
                    "the daemon that answered is not this configuration's: {detail}"
                )
            }
            Self::VersionMismatch { daemon } => write!(
                f,
                "the running daemon is version {daemon} but this CLI is {CLI_VERSION}"
            ),
            Self::HandshakeRefused { code, message } => {
                write!(
                    f,
                    "the daemon refused this caller at initialize ({code}): {message}"
                )
            }
            Self::RequestRefused { code, message } => write!(
                f,
                "the daemon bound this caller to a principal and refused it the request ({code}): {message}"
            ),
            Self::Rejected { code, message } => {
                write!(f, "the daemon rejected the request ({code}): {message}")
            }
            Self::NoAnswer { detail } => {
                write!(f, "the daemon did not answer the request: {detail}")
            }
            #[cfg(windows)]
            Self::UnverifiableEndpoint { detail } => write!(
                f,
                "which process serves the daemon's named pipe could not be verified, so nothing was sent: {detail}"
            ),
        }
    }
}

impl std::error::Error for DaemonCallError {}

/// What the platform established about the process serving an open
/// endpoint, before anything is sent over it.
#[derive(Debug, Clone)]
struct ServerPeer {
    /// Why the endpoint's owner may not be this configuration's daemon, when
    /// it may not; `None` when it may: this account's process, or the
    /// system's.
    foreign_owner: Option<String>,
    /// The server's pid, where the platform reports one.
    pid: Option<u32>,
}

/// Check the peer, run the handshake, then send the request, over an
/// open stream. `endpoint` names the stream in errors, `peer` is what the
/// platform established about its other end, and `expected_pid` is the
/// daemon pid this configuration's heartbeat records.
///
/// The owner check keeps the request away from another account's process;
/// the pid check keeps it away from another daemon of this account, a
/// misrouting rather than an attack, since a process of this account is
/// already inside this account's trust boundary.
async fn exchange<S>(
    stream: S,
    endpoint: &Path,
    peer: ServerPeer,
    expected_pid: u32,
    method: &str,
    params: Value,
) -> Result<Value, DaemonCallError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // An endpoint another account owns may be squatted, so it is sent
    // nothing at all, not even the handshake.
    if let Some(mismatch) = peer.foreign_owner {
        return Err(other_daemon(endpoint, mismatch));
    }
    // A process the platform names is identified before anything is
    // written: one that is not this configuration's daemon, e.g. the one
    // whose endpoint `ZEROCLAW_SOCKET` names, is sent nothing at all.
    if let Some(pid) = peer.pid
        && pid != expected_pid
    {
        return Err(other_daemon(
            endpoint,
            format!(
                "the kernel reports its pid as {pid}, but this configuration's daemon is pid {expected_pid}"
            ),
        ));
    }
    // The client presents no credential of its own: the daemon identifies
    // it by the endpoint's peer credential. It forwards no environment and
    // claims no TUI identity either, so the protocol version is all it
    // sends.
    let initialize = encode(
        INITIALIZE_ID,
        INITIALIZE_METHOD,
        serde_json::json!({ "protocol_version": RPC_PROTOCOL_VERSION }),
    )?;
    let request = encode(REQUEST_ID, method, params)?;
    let mut stream = BufReader::new(stream);

    let answer = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        round_trip(&mut stream, &initialize, INITIALIZE_ID),
    )
    .await
    .map_err(|_| {
        handshake_failed(format!(
            "no answer to initialize within {}s",
            HANDSHAKE_TIMEOUT.as_secs()
        ))
    })?
    .map_err(handshake_failed)?;
    let accepted = answer.map_err(|error| {
        if is_refusal(&error) {
            DaemonCallError::HandshakeRefused {
                code: i64::from(error.code),
                message: error.message,
            }
        } else {
            handshake_failed(format!(
                "initialize failed ({}): {}",
                error.code, error.message
            ))
        }
    })?;
    let accepted: InitializeResult = serde_json::from_value(accepted)
        .map_err(|error| handshake_failed(format!("unreadable initialize result: {error}")))?;
    // Where the platform records no pid, the one the daemon reports at
    // `initialize` is the only one to compare. A daemon whose pid is not
    // the one this configuration records serves another configuration and
    // must not receive this configuration's edit, which may carry a
    // secret.
    if peer.pid.is_none() && accepted.server_pid != expected_pid {
        return Err(other_daemon(
            endpoint,
            format!(
                "its pid is {}, but this configuration's daemon is pid {expected_pid}",
                accepted.server_pid
            ),
        ));
    }
    if accepted.server_version != CLI_VERSION {
        return Err(DaemonCallError::VersionMismatch {
            daemon: accepted.server_version,
        });
    }

    // The daemon may act on the request as soon as any of it is written,
    // so from here on a missing answer leaves the outcome unknown.
    let answer = tokio::time::timeout(
        REQUEST_TIMEOUT,
        round_trip(&mut stream, &request, REQUEST_ID),
    )
    .await
    .map_err(|_| DaemonCallError::NoAnswer {
        detail: format!("no answer within {}s", REQUEST_TIMEOUT.as_secs()),
    })?
    .map_err(|detail| DaemonCallError::NoAnswer { detail })?;
    // `initialize` bound a principal, so a refusal here is the daemon's
    // verdict on what that principal may do, not on who the caller is.
    answer.map_err(|error| {
        if is_refusal(&error) {
            DaemonCallError::RequestRefused {
                code: i64::from(error.code),
                message: error.message,
            }
        } else {
            DaemonCallError::Rejected {
                code: i64::from(error.code),
                message: error.message,
            }
        }
    })
}

/// One newline-terminated request frame.
fn encode(id: &str, method: &str, params: Value) -> Result<String, DaemonCallError> {
    let request = JsonRpcRequest::new(method, params, Value::String(id.to_owned()));
    let mut frame = serde_json::to_string(&request)
        .map_err(|error| handshake_failed(format!("failed to encode {method}: {error}")))?;
    frame.push('\n');
    Ok(frame)
}

/// Write `frame`, then read until the answer carrying `id` arrives.
/// Frames that are not that answer (notifications, requests from the
/// daemon, answers to other ids) are skipped. `Err` describes a transport
/// or protocol failure.
async fn round_trip<S>(
    stream: &mut BufReader<S>,
    frame: &str,
    id: &str,
) -> Result<Result<Value, JsonRpcError>, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(frame.as_bytes())
        .await
        .map_err(|error| format!("writing to the daemon failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("writing to the daemon failed: {error}"))?;
    let mut line = String::new();
    loop {
        line.clear();
        let read = (&mut *stream)
            .take(MAX_FRAME_BYTES + 1)
            .read_line(&mut line)
            .await
            .map_err(|error| format!("reading from the daemon failed: {error}"))?;
        if read == 0 {
            return Err("the daemon closed the connection".to_owned());
        }
        if read as u64 > MAX_FRAME_BYTES {
            return Err(format!(
                "the daemon sent a frame over {MAX_FRAME_BYTES} bytes"
            ));
        }
        // A blank line carries no frame; the daemon's own reader skips
        // them too.
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = serde_json::from_str(&line)
            .map_err(|error| format!("the daemon sent invalid JSON: {error}"))?;
        match JsonRpcFrame::from_value(frame) {
            Ok(JsonRpcFrame::Response {
                id: answered,
                result,
            }) if answered.as_str() == Some(id) => return Ok(result),
            Ok(_) => {}
            Err(error) => return Err(format!("the daemon sent an invalid frame: {error}")),
        }
    }
}

/// AUTH_REQUIRED and FORBIDDEN concern the caller and its principal
/// rather than the request's content.
fn is_refusal(error: &JsonRpcError) -> bool {
    matches!(error.code, AUTH_REQUIRED | FORBIDDEN)
}

fn handshake_failed(detail: String) -> DaemonCallError {
    DaemonCallError::Handshake { detail }
}

fn other_daemon(endpoint: &Path, mismatch: String) -> DaemonCallError {
    DaemonCallError::OtherDaemon {
        detail: format!("{}: {mismatch}", endpoint.display()),
    }
}

/// The decision about a named pipe's server, kept apart from the Win32
/// queries that feed it so it can be checked on every platform.
#[cfg(any(windows, test))]
mod pipe {
    use zeroclaw_config::platform::pipe_server::ProcessAccount;

    /// Why a pipe's server was sent nothing.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Rejection {
        /// The server is another process, or runs under another account.
        OtherServer(String),
        /// A check could not be completed, so nothing is known about the
        /// server.
        Unverifiable(String),
    }

    /// The pid of the pipe's server, once it is shown to be the process
    /// `expected_pid` names, running as this account or SYSTEM. `server_pid`
    /// asks the kernel which process created the server end; `account_of`,
    /// which account a process runs under. The pid is checked first, so a
    /// server that is another process is never opened.
    pub(super) fn verify_server(
        expected_pid: u32,
        server_pid: impl FnOnce() -> Result<u32, String>,
        account_of: impl FnOnce(u32) -> Result<ProcessAccount, String>,
    ) -> Result<u32, Rejection> {
        let pid = server_pid().map_err(|error| {
            Rejection::Unverifiable(format!(
                "the pipe's server process could not be read: {error}"
            ))
        })?;
        if pid != expected_pid {
            return Err(Rejection::OtherServer(format!(
                "the kernel reports its server as pid {pid}, but this configuration's daemon is pid {expected_pid}"
            )));
        }
        match account_of(pid) {
            Ok(ProcessAccount::ThisUser | ProcessAccount::System) => Ok(pid),
            Ok(ProcessAccount::Other) => Err(Rejection::OtherServer(format!(
                "its server process {pid} runs under an account that is neither this one nor SYSTEM"
            ))),
            Err(error) => Err(Rejection::Unverifiable(format!(
                "the account of server process {pid} could not be read: {error}"
            ))),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::Cell;

        const DAEMON: u32 = 4242;

        /// The account query, recording whether it was made.
        fn account(
            answer: Result<ProcessAccount, String>,
            asked: &Cell<Option<u32>>,
        ) -> impl FnOnce(u32) -> Result<ProcessAccount, String> {
            move |pid| {
                asked.set(Some(pid));
                answer
            }
        }

        #[test]
        fn verify_server_accepts_the_recorded_daemon_run_as_this_user_or_system() {
            for owner in [ProcessAccount::ThisUser, ProcessAccount::System] {
                let asked = Cell::new(None);
                assert_eq!(
                    verify_server(DAEMON, || Ok(DAEMON), account(Ok(owner), &asked)),
                    Ok(DAEMON),
                    "{owner:?}"
                );
                assert_eq!(asked.get(), Some(DAEMON));
            }
        }

        #[test]
        fn verify_server_rejects_another_pid_without_opening_it() {
            let asked = Cell::new(None);
            let rejection = verify_server(
                DAEMON,
                || Ok(DAEMON + 1),
                account(Ok(ProcessAccount::ThisUser), &asked),
            )
            .expect_err("another process is not the daemon");
            match rejection {
                Rejection::OtherServer(detail) => {
                    assert!(detail.contains(&format!("pid {}", DAEMON + 1)), "{detail}");
                    assert!(detail.contains(&format!("pid {DAEMON}")), "{detail}");
                }
                Rejection::Unverifiable(detail) => {
                    panic!("expected OtherServer, got Unverifiable({detail})")
                }
            }
            assert_eq!(asked.get(), None, "a mismatched pid is never opened");
        }

        #[test]
        fn verify_server_rejects_another_account() {
            let asked = Cell::new(None);
            let rejection = verify_server(
                DAEMON,
                || Ok(DAEMON),
                account(Ok(ProcessAccount::Other), &asked),
            )
            .expect_err("another account may not serve this configuration");
            match rejection {
                Rejection::OtherServer(detail) => {
                    assert!(detail.contains(&format!("process {DAEMON}")), "{detail}");
                    assert!(detail.contains("neither this one nor SYSTEM"), "{detail}");
                }
                Rejection::Unverifiable(detail) => {
                    panic!("expected OtherServer, got Unverifiable({detail})")
                }
            }
        }

        #[test]
        fn verify_server_fails_closed_when_the_pid_cannot_be_read() {
            let asked = Cell::new(None);
            let rejection = verify_server(
                DAEMON,
                || Err("GetNamedPipeServerProcessId: access denied".to_owned()),
                account(Ok(ProcessAccount::ThisUser), &asked),
            )
            .expect_err("an unknown server is not sent anything");
            assert_eq!(
                rejection,
                Rejection::Unverifiable(
                    "the pipe's server process could not be read: GetNamedPipeServerProcessId: access denied".to_owned()
                )
            );
            assert_eq!(asked.get(), None);
        }

        #[test]
        fn verify_server_fails_closed_when_the_account_cannot_be_read() {
            let asked = Cell::new(None);
            let rejection = verify_server(
                DAEMON,
                || Ok(DAEMON),
                account(Err("OpenProcess(4242): access denied".to_owned()), &asked),
            )
            .expect_err("a server of unknown account is not sent anything");
            assert_eq!(
                rejection,
                Rejection::Unverifiable(
                    "the account of server process 4242 could not be read: OpenProcess(4242): access denied".to_owned()
                )
            );
        }
    }
}

/// The client where the kernel reports who serves the endpoint.
#[cfg(unix)]
mod unix {
    use std::path::Path;

    use serde_json::Value;

    use super::{DaemonCallError, ServerPeer, exchange, other_daemon};
    use crate::config::Config;

    /// Connect, check that the process serving the endpoint is the daemon
    /// `expected_pid` names, `initialize`, send one request, return its
    /// `result`. `expected_pid` is the pid this configuration's heartbeat
    /// records: the heartbeat sits beside config.toml whatever endpoint
    /// `ZEROCLAW_SOCKET` names.
    pub(crate) async fn call(
        config: &Config,
        expected_pid: u32,
        method: &str,
        params: Value,
    ) -> Result<Value, DaemonCallError> {
        let path = zeroclaw_runtime::rpc::local::socket_path(config);
        let stream = match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => stream,
            Err(source) => return Err(DaemonCallError::Unavailable { path, source }),
        };
        let peer = peer_credential(&stream, &path)?;
        exchange(stream, &path, peer, expected_pid, method, params).await
    }

    /// The account and process the kernel reports at the other end of the
    /// endpoint. A peer whose credential cannot be read cannot be shown to be
    /// this configuration's daemon.
    fn peer_credential(
        stream: &tokio::net::UnixStream,
        path: &Path,
    ) -> Result<ServerPeer, DaemonCallError> {
        let credential = stream
            .peer_cred()
            .map_err(|error| other_daemon(path, format!("its owner could not be read: {error}")))?;
        Ok(server_peer(
            credential.uid(),
            credential.pid().and_then(|pid| u32::try_from(pid).ok()),
        ))
    }

    /// What `exchange` checks about a peer the kernel reports as `uid` and,
    /// where it records one, `pid`.
    fn server_peer(uid: u32, pid: Option<u32>) -> ServerPeer {
        ServerPeer {
            foreign_owner: (!owner_may_serve(uid)).then(|| {
                format!("it is owned by uid {uid}, which is neither this account nor root")
            }),
            pid,
        }
    }

    /// Whether an endpoint owned by `uid` may be this configuration's daemon:
    /// one this account runs, or one root runs.
    fn owner_may_serve(uid: u32) -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let own = unsafe { libc::geteuid() };
        uid == own || uid == 0
    }

    #[cfg(test)]
    mod tests {
        use super::super::support::{
            SCRIPTED_PID, initialize_answer, read_frame, scratch_config, scripted_endpoint, serve,
            write_frames,
        };
        use super::super::{CLI_VERSION, exchange};
        use super::*;
        use serde_json::json;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, INVALID_PARAMS};

        /// This process's account, which may serve this configuration's
        /// daemon.
        fn own_uid() -> u32 {
            // SAFETY: geteuid has no preconditions and cannot fail.
            unsafe { libc::geteuid() }
        }

        #[tokio::test]
        async fn call_returns_the_result_of_the_request() {
            let (_dir, config) = scratch_config();
            let _daemon = serve(&config).await;

            let result = call(
                &config,
                std::process::id(),
                "config/get",
                json!({ "prop": "gateway.host" }),
            )
            .await
            .expect("config/get answers");
            assert_eq!(result["value"], json!(config.gateway.host));
        }

        /// The listener runs in this process, so the process serving the
        /// endpoint is this one. A call that expects another pid, as for a
        /// configuration whose endpoint `ZEROCLAW_SOCKET` sends elsewhere,
        /// sends it nothing, so the edit it would have applied leaves no
        /// trace.
        #[tokio::test]
        async fn call_sends_nothing_to_a_process_other_than_the_expected_daemon() {
            let (dir, config) = scratch_config();
            let _daemon = serve(&config).await;
            let edit = || json!({ "prop": "gateway.host", "value": "127.0.0.2" });
            let endpoint = dir.path().join("daemon.sock").display().to_string();
            let own = std::process::id();
            let elsewhere = own.wrapping_add(1);

            match call(&config, elsewhere, "config/set", edit()).await {
                Err(DaemonCallError::OtherDaemon { detail }) => {
                    assert!(detail.starts_with(&endpoint), "{detail}");
                    // As the kernel reports it, or, where the kernel records
                    // no pid, as the listener reports it.
                    assert!(
                        detail.contains(&format!("its pid as {own}"))
                            || detail.contains(&format!("its pid is {own}")),
                        "the mismatch names this process: {detail}"
                    );
                    assert!(detail.contains(&format!("pid {elsewhere}")), "{detail}");
                }
                other => panic!("expected OtherDaemon for another pid, got {other:?}"),
            }
            assert!(
                !config.config_path.exists(),
                "no request may reach a process that is not the expected daemon"
            );

            call(&config, own, "config/set", edit())
                .await
                .expect("the expected daemon takes the edit");
            assert!(
                config.config_path.exists(),
                "the expected daemon saves the edit"
            );
        }

        #[tokio::test]
        async fn call_without_a_listener_is_unavailable() {
            let (dir, config) = scratch_config();

            match call(&config, std::process::id(), "config/get", json!({})).await {
                Err(DaemonCallError::Unavailable { path, source }) => {
                    assert_eq!(path, dir.path().join("daemon.sock"));
                    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }

        /// A roster entry without a permission profile fails the daemon's
        /// authorization validation: the daemon answers with its reason and
        /// writes nothing.
        #[tokio::test]
        async fn call_reports_a_rejected_request_and_the_daemon_saves_nothing() {
            let (_dir, config) = scratch_config();
            let _daemon = serve(&config).await;

            match call(
                &config,
                std::process::id(),
                "config/set",
                json!({ "prop": "users.alice.uid", "value": "4242" }),
            )
            .await
            {
                Err(DaemonCallError::Rejected { code, message }) => {
                    assert_eq!(code, i64::from(INVALID_PARAMS));
                    assert!(message.contains("users.alice"), "{message}");
                }
                other => panic!("expected Rejected, got {other:?}"),
            }
            assert!(
                !config.config_path.exists(),
                "a rejected edit must not reach config.toml"
            );
        }

        /// With daemon-uid trust off and no roster, the endpoint's peer
        /// credential identifies nobody, so the daemon refuses the handshake.
        #[tokio::test]
        async fn call_reports_a_caller_refused_at_the_handshake() {
            let (_dir, mut config) = scratch_config();
            config.security.trust_daemon_uid = false;
            let _daemon = serve(&config).await;

            match call(&config, std::process::id(), "config/get", json!({})).await {
                Err(DaemonCallError::HandshakeRefused { code, .. }) => {
                    assert_eq!(code, i64::from(AUTH_REQUIRED));
                }
                other => panic!("expected HandshakeRefused, got {other:?}"),
            }
        }

        /// An endpoint another account owns is sent nothing, not even the
        /// handshake.
        #[tokio::test]
        async fn exchange_sends_nothing_to_an_endpoint_another_account_owns() {
            let stranger = if own_uid() == 1 { 2 } else { 1 };
            let (client, daemon_end) = tokio::io::duplex(64 * 1024);
            let daemon = async move {
                let mut daemon_end = BufReader::new(daemon_end);
                let mut first = String::new();
                daemon_end
                    .read_line(&mut first)
                    .await
                    .expect("the client hangs up cleanly");
                first
            };

            let (result, first) = tokio::join!(
                exchange(
                    client,
                    scripted_endpoint(),
                    server_peer(stranger, Some(SCRIPTED_PID)),
                    SCRIPTED_PID,
                    "config/set",
                    json!({}),
                ),
                daemon
            );
            match result {
                Err(DaemonCallError::OtherDaemon { detail }) => {
                    assert!(detail.starts_with("scripted.sock: "), "{detail}");
                    assert!(detail.contains(&format!("uid {stranger}")), "{detail}");
                }
                other => panic!("expected OtherDaemon, got {other:?}"),
            }
            assert!(first.is_empty(), "nothing may be sent: {first}");
        }

        /// An endpoint this account owns, or root owns, gets the request.
        #[tokio::test]
        async fn exchange_trusts_an_endpoint_this_account_or_root_owns() {
            for owner in [own_uid(), 0] {
                let (client, daemon_end) = tokio::io::duplex(64 * 1024);
                let daemon = async move {
                    let mut daemon_end = BufReader::new(daemon_end);
                    read_frame(&mut daemon_end).await;
                    write_frames(&mut daemon_end, &[initialize_answer(CLI_VERSION)]).await;
                    let request = read_frame(&mut daemon_end).await;
                    write_frames(
                        &mut daemon_end,
                        &[json!({ "jsonrpc": "2.0", "id": request["id"], "result": { "owner": owner } })],
                    )
                    .await;
                };

                let (result, ()) = tokio::join!(
                    exchange(
                        client,
                        scripted_endpoint(),
                        server_peer(owner, None),
                        SCRIPTED_PID,
                        "config/get",
                        json!({}),
                    ),
                    daemon
                );
                assert_eq!(
                    result.unwrap_or_else(|error| panic!("uid {owner} may serve: {error}")),
                    json!({ "owner": owner })
                );
            }
        }
    }
}

/// The client where the kernel reports which process created the pipe's
/// server end, and that process's token says whose it is.
#[cfg(windows)]
mod windows {
    use serde_json::Value;
    use zeroclaw_config::platform::pipe_server::{process_account, server_process_id};

    use super::{DaemonCallError, ServerPeer, exchange, other_daemon, pipe};
    use crate::config::Config;

    /// Open the pipe, check that the process serving it is the daemon
    /// `expected_pid` names and runs as this account or SYSTEM, `initialize`,
    /// send one request, return its `result`. `expected_pid` is the pid this
    /// configuration's heartbeat records: the heartbeat sits beside
    /// config.toml whatever endpoint `ZEROCLAW_SOCKET` names.
    pub(crate) async fn call(
        config: &Config,
        expected_pid: u32,
        method: &str,
        params: Value,
    ) -> Result<Value, DaemonCallError> {
        let path = zeroclaw_runtime::rpc::local::socket_path(config);
        let client = match zeroclaw_runtime::rpc::local::connect_client(&path).await {
            Ok(client) => client,
            Err(source) => return Err(DaemonCallError::Unavailable { path, source }),
        };
        let pid = pipe::verify_server(
            expected_pid,
            || server_process_id(&client).map_err(|error| error.to_string()),
            |pid| process_account(pid).map_err(|error| error.to_string()),
        )
        .map_err(|rejection| match rejection {
            pipe::Rejection::OtherServer(detail) => other_daemon(&path, detail),
            pipe::Rejection::Unverifiable(detail) => DaemonCallError::UnverifiableEndpoint {
                detail: format!("{}: {detail}", path.display()),
            },
        })?;
        // Both checks passed before the handshake, so `exchange` has only
        // the pid to compare, and the kernel's answer stands over whatever
        // the server claims at `initialize`.
        let peer = ServerPeer {
            foreign_owner: None,
            pid: Some(pid),
        };
        exchange(client, &path, peer, expected_pid, method, params).await
    }

    #[cfg(test)]
    mod tests {
        use super::super::support::{scratch_config, serve};
        use super::*;
        use serde_json::json;
        use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, INVALID_PARAMS};
        use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};
        use zeroclaw_runtime::rpc::local::socket_path;

        #[tokio::test]
        async fn call_returns_the_result_of_the_request() {
            let (_dir, config) = scratch_config();
            let _daemon = serve(&config).await;

            let result = call(
                &config,
                std::process::id(),
                "config/get",
                json!({ "prop": "gateway.host" }),
            )
            .await
            .expect("config/get answers");
            assert_eq!(result["value"], json!(config.gateway.host));
        }

        /// The listener runs in this process, so the kernel names this
        /// process as the pipe's server. A call that expects another pid, as
        /// for a configuration whose endpoint `ZEROCLAW_SOCKET` sends
        /// elsewhere, sends it nothing, so the edit it would have applied
        /// leaves no trace.
        #[tokio::test]
        async fn call_sends_nothing_to_a_process_other_than_the_expected_daemon() {
            let (_dir, config) = scratch_config();
            let _daemon = serve(&config).await;
            let edit = || json!({ "prop": "gateway.host", "value": "127.0.0.2" });
            let endpoint = socket_path(&config).display().to_string();
            let own = std::process::id();
            let elsewhere = own.wrapping_add(1);

            match call(&config, elsewhere, "config/set", edit()).await {
                Err(DaemonCallError::OtherDaemon { detail }) => {
                    assert!(detail.starts_with(&endpoint), "{detail}");
                    assert!(
                        detail.contains(&format!("server as pid {own}")),
                        "the mismatch names this process: {detail}"
                    );
                    assert!(detail.contains(&format!("pid {elsewhere}")), "{detail}");
                }
                other => panic!("expected OtherDaemon for another pid, got {other:?}"),
            }
            assert!(
                !config.config_path.exists(),
                "no request may reach a process that is not the expected daemon"
            );

            call(&config, own, "config/set", edit())
                .await
                .expect("the expected daemon takes the edit");
            assert!(
                config.config_path.exists(),
                "the expected daemon saves the edit"
            );
        }

        #[tokio::test]
        async fn call_without_a_listener_is_unavailable() {
            let (_dir, config) = scratch_config();

            match call(&config, std::process::id(), "config/get", json!({})).await {
                Err(DaemonCallError::Unavailable { path, source }) => {
                    assert_eq!(path, socket_path(&config));
                    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }

        /// A roster entry without a permission profile fails the daemon's
        /// authorization validation: the daemon answers with its reason and
        /// writes nothing.
        #[tokio::test]
        async fn call_reports_a_rejected_request_and_the_daemon_saves_nothing() {
            let (_dir, config) = scratch_config();
            let _daemon = serve(&config).await;

            match call(
                &config,
                std::process::id(),
                "config/set",
                json!({ "prop": "users.alice.uid", "value": "4242" }),
            )
            .await
            {
                Err(DaemonCallError::Rejected { code, message }) => {
                    assert_eq!(code, i64::from(INVALID_PARAMS));
                    assert!(message.contains("users.alice"), "{message}");
                }
                other => panic!("expected Rejected, got {other:?}"),
            }
            assert!(
                !config.config_path.exists(),
                "a rejected edit must not reach config.toml"
            );
        }

        /// A pipe carries no peer uid, so once a roster exists a client that
        /// presents no token identifies nobody, and the daemon refuses the
        /// handshake.
        #[tokio::test]
        async fn call_reports_a_caller_refused_at_the_handshake() {
            let (_dir, mut config) = scratch_config();
            config
                .permission_profiles
                .insert("reader".into(), PermissionProfileConfig::default());
            config.users.insert(
                "alice".into(),
                UserConfig {
                    principal_id: None,
                    uid: Some(4242),
                    permission_profiles: vec!["reader".into()],
                },
            );
            let _daemon = serve(&config).await;

            match call(&config, std::process::id(), "config/get", json!({})).await {
                Err(DaemonCallError::HandshakeRefused { code, .. }) => {
                    assert_eq!(code, i64::from(AUTH_REQUIRED));
                }
                other => panic!("expected HandshakeRefused, got {other:?}"),
            }
        }
    }
}

/// Fixtures shared by the tests of both clients, of `exchange`, and of the
/// publication that calls them.
#[cfg(test)]
pub(crate) mod support {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

    use super::{INITIALIZE_ID, RPC_PROTOCOL_VERSION, ServerPeer};
    use crate::config::Config;

    /// The pid every scripted daemon answers `initialize` with.
    pub(super) const SCRIPTED_PID: u32 = 1;

    /// The name a scripted stream goes by in errors.
    pub(super) fn scripted_endpoint() -> &'static Path {
        Path::new("scripted.sock")
    }

    /// A peer that may serve this configuration, for which the platform
    /// records no pid.
    pub(super) fn unnamed_peer() -> ServerPeer {
        ServerPeer {
            foreign_owner: None,
            pid: None,
        }
    }

    /// A config whose endpoint lives in a fresh directory. On Unix the
    /// directory sits under /tmp because macOS caps a unix socket path at
    /// 104 bytes.
    pub(crate) fn scratch_config() -> (tempfile::TempDir, Config) {
        assert!(
            std::env::var_os("ZEROCLAW_SOCKET").is_none(),
            "ZEROCLAW_SOCKET redirects the endpoint away from these fixtures; unset it"
        );
        #[cfg(unix)]
        let dir = tempfile::Builder::new().prefix("zc").tempdir_in("/tmp");
        #[cfg(not(unix))]
        let dir = tempfile::tempdir();
        let dir = dir.expect("a scratch directory");
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            config_path: dir.path().join("config.toml"),
            ..Config::default()
        };
        (dir, config)
    }

    /// Serve `config`'s endpoint with the daemon's own RPC listener until
    /// the returned guard drops. The listener runs in this process, so
    /// the kernel names this process at its end of the endpoint, and it
    /// answers `initialize` with this process's pid.
    pub(crate) async fn serve(config: &Config) -> tokio_util::sync::DropGuard {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(16, queue));
        let ctx =
            zeroclaw_runtime::rpc::context::RpcContext::for_live_test(config.clone(), sessions);
        let cancel = tokio_util::sync::CancellationToken::new();
        let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(false);
        let readiness = zeroclaw_runtime::daemon::SocketReadinessReporter::new(move || {
            let _ = ready_tx.send(true);
        });
        let listener_cancel = cancel.clone();
        zeroclaw_spawn::spawn!(async move {
            zeroclaw_runtime::rpc::local::run_local_listener(
                ctx,
                listener_cancel,
                Arc::new(AtomicUsize::new(0)),
                Some(readiness),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), ready_rx.wait_for(|ready| *ready))
            .await
            .expect("the listener binds within 5s")
            .expect("the listener binds its endpoint");
        cancel.drop_guard()
    }

    pub(super) async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> Value {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("the client writes a frame");
        serde_json::from_str(&line).expect("the client writes JSON")
    }

    pub(super) async fn write_frames<W: AsyncWrite + Unpin>(writer: &mut W, frames: &[Value]) {
        for frame in frames {
            writer
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .expect("the client is reading");
        }
    }

    pub(super) fn initialize_answer(server_version: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": INITIALIZE_ID,
            "result": {
                "protocol_version": RPC_PROTOCOL_VERSION,
                "server_version": server_version,
                "server_pid": SCRIPTED_PID,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::support::*;
    use super::*;
    use serde_json::json;
    use zeroclaw_api::jsonrpc::error_codes::VERSION_MISMATCH;

    /// `exchange` with a scripted daemon that may serve this configuration,
    /// for which the platform records no pid, and whose own pid is the one
    /// this configuration records.
    async fn exchange_with_recorded(
        client: tokio::io::DuplexStream,
        method: &str,
    ) -> Result<Value, DaemonCallError> {
        exchange(
            client,
            scripted_endpoint(),
            unnamed_peer(),
            SCRIPTED_PID,
            method,
            json!({}),
        )
        .await
    }

    #[tokio::test]
    async fn exchange_skips_frames_that_do_not_answer_it() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            let initialize = read_frame(&mut daemon_end).await;
            assert_eq!(initialize["method"], INITIALIZE_METHOD);
            assert_eq!(
                initialize["params"],
                json!({ "protocol_version": RPC_PROTOCOL_VERSION }),
                "the handshake carries the protocol version and nothing else"
            );
            daemon_end
                .write_all(b"\n  \r\n")
                .await
                .expect("the client is reading");
            write_frames(
                &mut daemon_end,
                &[
                    json!({ "jsonrpc": "2.0", "method": "session/update", "params": {} }),
                    initialize_answer(CLI_VERSION),
                ],
            )
            .await;
            let request = read_frame(&mut daemon_end).await;
            assert_eq!(request["method"], "config/get");
            daemon_end
                .write_all(b"\n")
                .await
                .expect("the client is reading");
            write_frames(
                &mut daemon_end,
                &[
                    json!({ "jsonrpc": "2.0", "id": "another", "result": { "value": "theirs" } }),
                    json!({ "jsonrpc": "2.0", "method": "session/update", "params": {} }),
                    json!({ "jsonrpc": "2.0", "id": request["id"], "result": { "value": "ours" } }),
                ],
            )
            .await;
        };

        let (result, ()) = tokio::join!(exchange_with_recorded(client, "config/get"), daemon);
        assert_eq!(
            result.expect("blank lines and other frames are skipped"),
            json!({ "value": "ours" })
        );
    }

    /// Where the platform records no pid, the pid the daemon reports at
    /// `initialize` must be the one this configuration records: another
    /// gets nothing past the handshake.
    #[tokio::test]
    async fn exchange_sends_the_request_only_to_the_recorded_daemon() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            read_frame(&mut daemon_end).await;
            write_frames(&mut daemon_end, &[initialize_answer(CLI_VERSION)]).await;
            let mut rest = String::new();
            daemon_end
                .read_line(&mut rest)
                .await
                .expect("the client hangs up cleanly");
            rest
        };

        let expected = SCRIPTED_PID + 1;
        let (result, rest) = tokio::join!(
            exchange(
                client,
                scripted_endpoint(),
                unnamed_peer(),
                expected,
                "config/set",
                json!({}),
            ),
            daemon
        );
        match result {
            Err(DaemonCallError::OtherDaemon { detail }) => {
                assert!(detail.starts_with("scripted.sock: "), "{detail}");
                assert!(
                    detail.contains(&format!("its pid is {SCRIPTED_PID}")),
                    "{detail}"
                );
                assert!(detail.contains(&format!("pid {expected}")), "{detail}");
            }
            other => panic!("expected OtherDaemon, got {other:?}"),
        }
        assert!(rest.is_empty(), "the request must not be sent: {rest}");
    }

    /// Where the platform reports the peer's pid, that pid is what is
    /// compared. A peer it names as another process is sent nothing at
    /// all, not even the handshake, however right the pid it would claim.
    #[tokio::test]
    async fn exchange_sends_nothing_to_a_peer_the_kernel_names_as_another_process() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            let mut received = String::new();
            // Were the handshake sent, this daemon would claim the pid
            // this configuration records.
            if daemon_end
                .read_line(&mut received)
                .await
                .expect("the client hangs up cleanly")
                > 0
            {
                write_frames(&mut daemon_end, &[initialize_answer(CLI_VERSION)]).await;
                daemon_end
                    .read_line(&mut received)
                    .await
                    .expect("the client hangs up cleanly");
            }
            received
        };

        let kernel_pid = SCRIPTED_PID + 1;
        let peer = ServerPeer {
            foreign_owner: None,
            pid: Some(kernel_pid),
        };
        let (result, received) = tokio::join!(
            exchange(
                client,
                scripted_endpoint(),
                peer,
                SCRIPTED_PID,
                "config/set",
                json!({}),
            ),
            daemon
        );
        match result {
            Err(DaemonCallError::OtherDaemon { detail }) => {
                assert!(detail.starts_with("scripted.sock: "), "{detail}");
                assert!(
                    detail.contains(&format!("the kernel reports its pid as {kernel_pid}")),
                    "{detail}"
                );
            }
            other => panic!("expected OtherDaemon, got {other:?}"),
        }
        assert!(received.is_empty(), "nothing may be sent: {received}");
    }

    /// A peer the platform reports as another account's is sent nothing,
    /// not even the handshake, whatever pid it has.
    #[tokio::test]
    async fn exchange_sends_nothing_to_a_peer_another_account_owns() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            let mut first = String::new();
            daemon_end
                .read_line(&mut first)
                .await
                .expect("the client hangs up cleanly");
            first
        };

        let peer = ServerPeer {
            foreign_owner: Some("it is owned by another account".to_owned()),
            pid: Some(SCRIPTED_PID),
        };
        let (result, first) = tokio::join!(
            exchange(
                client,
                scripted_endpoint(),
                peer,
                SCRIPTED_PID,
                "config/set",
                json!({}),
            ),
            daemon
        );
        match result {
            Err(DaemonCallError::OtherDaemon { detail }) => {
                assert_eq!(detail, "scripted.sock: it is owned by another account");
            }
            other => panic!("expected OtherDaemon, got {other:?}"),
        }
        assert!(first.is_empty(), "nothing may be sent: {first}");
    }

    /// A peer the kernel names as the recorded daemon gets the request
    /// even when it reports another pid at `initialize`: the kernel's
    /// report identifies it.
    #[tokio::test]
    async fn exchange_trusts_the_kernel_pid_over_the_pid_the_daemon_reports() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            read_frame(&mut daemon_end).await;
            let mut answer = initialize_answer(CLI_VERSION);
            answer["result"]["server_pid"] = json!(SCRIPTED_PID + 1);
            write_frames(&mut daemon_end, &[answer]).await;
            let request = read_frame(&mut daemon_end).await;
            write_frames(
                &mut daemon_end,
                &[json!({ "jsonrpc": "2.0", "id": request["id"], "result": { "set": true } })],
            )
            .await;
        };

        let peer = ServerPeer {
            foreign_owner: None,
            pid: Some(SCRIPTED_PID),
        };
        let (result, ()) = tokio::join!(
            exchange(
                client,
                scripted_endpoint(),
                peer,
                SCRIPTED_PID,
                "config/set",
                json!({}),
            ),
            daemon
        );
        assert_eq!(
            result.expect("the kernel names the recorded daemon"),
            json!({ "set": true })
        );
    }

    #[tokio::test]
    async fn exchange_sends_nothing_to_a_daemon_of_another_release() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            read_frame(&mut daemon_end).await;
            write_frames(&mut daemon_end, &[initialize_answer("0.0.1")]).await;
            let mut rest = String::new();
            daemon_end
                .read_line(&mut rest)
                .await
                .expect("the client hangs up cleanly");
            rest
        };

        let (result, rest) = tokio::join!(exchange_with_recorded(client, "config/set"), daemon);
        match result {
            Err(DaemonCallError::VersionMismatch { daemon }) => assert_eq!(daemon, "0.0.1"),
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        assert!(rest.is_empty(), "the request must not be sent: {rest}");
    }

    #[tokio::test]
    async fn exchange_reports_an_initialize_error_as_a_failed_handshake() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            read_frame(&mut daemon_end).await;
            write_frames(
                &mut daemon_end,
                &[json!({
                    "jsonrpc": "2.0",
                    "id": INITIALIZE_ID,
                    "error": { "code": VERSION_MISMATCH, "message": "protocol 2 only" },
                })],
            )
            .await;
        };

        let (result, ()) = tokio::join!(exchange_with_recorded(client, "config/set"), daemon);
        match result {
            Err(DaemonCallError::Handshake { detail }) => {
                assert!(detail.contains("protocol 2 only"), "{detail}");
            }
            other => panic!("expected Handshake, got {other:?}"),
        }
    }

    /// A refusal at `initialize` binds no principal, so the request is
    /// never sent, whichever refusal code the daemon answers.
    #[tokio::test]
    async fn exchange_reports_a_refusal_at_initialize_as_a_refused_handshake() {
        for refusal in [AUTH_REQUIRED, FORBIDDEN] {
            let (client, daemon_end) = tokio::io::duplex(64 * 1024);
            let daemon = async move {
                let mut daemon_end = BufReader::new(daemon_end);
                read_frame(&mut daemon_end).await;
                write_frames(
                    &mut daemon_end,
                    &[json!({
                        "jsonrpc": "2.0",
                        "id": INITIALIZE_ID,
                        "error": { "code": refusal, "message": "no principal for this uid" },
                    })],
                )
                .await;
                let mut rest = String::new();
                daemon_end
                    .read_line(&mut rest)
                    .await
                    .expect("the client hangs up cleanly");
                rest
            };

            let (result, rest) = tokio::join!(exchange_with_recorded(client, "config/set"), daemon);
            match result {
                Err(DaemonCallError::HandshakeRefused { code, message }) => {
                    assert_eq!(code, i64::from(refusal));
                    assert_eq!(message, "no principal for this uid");
                }
                other => panic!("expected HandshakeRefused for {refusal}, got {other:?}"),
            }
            assert!(rest.is_empty(), "the request must not be sent: {rest}");
        }
    }

    /// A refusal of the request after `initialize` bound a principal is
    /// told apart from a refusal at the handshake: it is the daemon's
    /// verdict on what that principal may do.
    #[tokio::test]
    async fn exchange_reports_a_refusal_after_the_handshake_as_a_refused_request() {
        for refusal in [AUTH_REQUIRED, FORBIDDEN] {
            let (client, daemon_end) = tokio::io::duplex(64 * 1024);
            let daemon = async move {
                let mut daemon_end = BufReader::new(daemon_end);
                read_frame(&mut daemon_end).await;
                write_frames(&mut daemon_end, &[initialize_answer(CLI_VERSION)]).await;
                let request = read_frame(&mut daemon_end).await;
                write_frames(
                    &mut daemon_end,
                    &[json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "error": { "code": refusal, "message": "Principal is not granted config:update" },
                    })],
                )
                .await;
            };

            let (result, ()) = tokio::join!(exchange_with_recorded(client, "config/set"), daemon);
            match result {
                Err(DaemonCallError::RequestRefused { code, message }) => {
                    assert_eq!(code, i64::from(refusal));
                    assert_eq!(message, "Principal is not granted config:update");
                }
                other => panic!("expected RequestRefused for {refusal}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn exchange_reports_no_answer_when_the_daemon_hangs_up_after_the_request() {
        let (client, daemon_end) = tokio::io::duplex(64 * 1024);
        let daemon = async move {
            let mut daemon_end = BufReader::new(daemon_end);
            read_frame(&mut daemon_end).await;
            write_frames(&mut daemon_end, &[initialize_answer(CLI_VERSION)]).await;
            read_frame(&mut daemon_end).await;
        };

        let (result, ()) = tokio::join!(exchange_with_recorded(client, "config/set"), daemon);
        assert!(
            matches!(result, Err(DaemonCallError::NoAnswer { .. })),
            "{result:?}"
        );
    }
}
