//! Startup checks after the app launches a daemon: the core over RPC, then the
//! dashboard's HTTP gateway, each with a deadline and a failure the splash can
//! name.
//!
//! The process ID and version the core reports are diagnostics. They decide
//! which failure to show, never whether a process is owned or trusted: the
//! endpoint's operating-system account is checked before the handshake, and
//! ownership comes only from the handle the launch returned.

use crate::gateway_client::GatewayClient;
use crate::possession::ProofFailure;
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;
use zeroclaw_rpc_client::{
    ClientError, ConnectOptions, Method, RPC_PROTOCOL_VERSION, RpcClient, error_codes,
};

/// How long the dashboard's HTTP gateway gets to answer `/health` after the
/// launched daemon is ready.
pub const GATEWAY_READY_DEADLINE: Duration = Duration::from_secs(60);
/// How long the RPC handshake with the launched core may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the gateway wait retries.
const GATEWAY_POLL: Duration = Duration::from_secs(1);
/// How many answers in a row without the core's proof mean another program
/// holds the dashboard address, rather than a gateway changing generation.
const PROOF_MISMATCHES_HELD: u32 = 3;

/// Why the daemon the app launched cannot be used, as the splash shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFailure {
    /// The daemon did not become ready before the deadline.
    Timeout(String),
    /// The core and this app do not speak compatible versions.
    Incompatible(String),
    /// Another process serves the daemon's RPC endpoint.
    EndpointHeld(String),
    /// Another process holds the dashboard's port.
    PortHeld(String),
    /// The launched core stopped answering before its gateway was ready.
    CoreUnavailable(String),
}

impl StartupFailure {
    /// The `zeroclaw://splash-status` kind for this failure.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Timeout(_) => "timeout",
            Self::Incompatible(_) => "incompatible",
            Self::EndpointHeld(_) => "endpoint_held",
            Self::PortHeld(_) => "port_held",
            Self::CoreUnavailable(_) => "core_unavailable",
        }
    }

    /// The message the splash shows.
    pub fn message(&self) -> &str {
        match self {
            Self::Timeout(message)
            | Self::Incompatible(message)
            | Self::EndpointHeld(message)
            | Self::PortHeld(message)
            | Self::CoreUnavailable(message) => message,
        }
    }

    /// Whether the launched daemon can never become usable, so the app stops
    /// it. A timeout is not final: the daemon may still finish starting.
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Timeout(_))
    }
}

/// What the core reported in its `initialize` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreHandshake {
    pub protocol_version: u64,
    pub server_version: String,
    pub server_pid: u32,
}

/// Check the core's handshake against this app. A bundled kernel ships with
/// this app from one build, so its version must equal the app's; a kernel
/// installed separately only has to speak this protocol. A process ID other
/// than the daemon the supervisor started means another core answered on
/// that endpoint.
pub fn check_handshake(
    handshake: &CoreHandshake,
    launched_pid: Option<u32>,
    bundled: bool,
    app_version: &str,
) -> Result<(), StartupFailure> {
    if handshake.protocol_version != RPC_PROTOCOL_VERSION {
        return Err(StartupFailure::Incompatible(format!(
            "The ZeroClaw core speaks protocol {}, but this app speaks protocol {RPC_PROTOCOL_VERSION}. Install a matching ZeroClaw and desktop app.",
            handshake.protocol_version
        )));
    }
    if bundled && handshake.server_version != app_version {
        return Err(StartupFailure::Incompatible(format!(
            "The ZeroClaw core bundled with this app is version {}, but the app is version {app_version}. Reinstall ZeroClaw Desktop.",
            handshake.server_version
        )));
    }
    if let Some(launched) = launched_pid
        && launched != handshake.server_pid
    {
        return Err(StartupFailure::EndpointHeld(format!(
            "Another ZeroClaw core (process {}) answers on the endpoint of the daemon this app started (process {launched}). Stop the other ZeroClaw and reopen the app.",
            handshake.server_pid
        )));
    }
    Ok(())
}

/// Dial the launched core's endpoint and check its handshake. On Unix the
/// endpoint must first prove, through the kernel, that it is served by this
/// app's own account. Windows cannot prove a pipe server's account yet, and
/// this dial carries no credential, so it is not gated there.
pub async fn verify_core(
    endpoint: &Path,
    launched_pid: Option<u32>,
    bundled: bool,
) -> Result<RpcClient, StartupFailure> {
    let options = ConnectOptions {
        handshake_timeout: Some(HANDSHAKE_TIMEOUT),
        verify_endpoint_owner: cfg!(unix),
        ..ConnectOptions::default()
    };
    // The handshake timeout starts only after the endpoint check; bound the
    // whole dial so a stalled connect or check cannot hold startup either.
    let client = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        RpcClient::connect_local(endpoint, options),
    )
    .await
    .map_err(|_| {
        StartupFailure::Timeout(format!(
            "The ZeroClaw core at {} did not complete its handshake within {} seconds.",
            endpoint.display(),
            HANDSHAKE_TIMEOUT.as_secs()
        ))
    })?
    .map_err(|error| classify_dial_error(endpoint, error))?;
    let handshake = client.handshake();
    check_handshake(
        &CoreHandshake {
            protocol_version: handshake.protocol_version,
            server_version: handshake.server_version.clone(),
            server_pid: handshake.server_pid,
        },
        launched_pid,
        bundled,
        env!("CARGO_PKG_VERSION"),
    )?;
    Ok(client)
}

fn classify_dial_error(endpoint: &Path, error: ClientError) -> StartupFailure {
    match error {
        ClientError::UntrustedEndpoint { rejection, .. } => StartupFailure::EndpointHeld(format!(
            "The ZeroClaw endpoint {} is not this user's ({rejection}). Stop the other process and reopen the app.",
            endpoint.display()
        )),
        ClientError::Rpc(rpc) if rpc.code == error_codes::VERSION_MISMATCH => {
            StartupFailure::Incompatible(format!(
                "The ZeroClaw core refused this app's protocol: {}. Install a matching ZeroClaw and desktop app.",
                rpc.message
            ))
        }
        other => StartupFailure::Timeout(format!(
            "The ZeroClaw core at {} did not complete its handshake: {other}",
            endpoint.display()
        )),
    }
}

/// The error a core reports for its HTTP gateway when the gateway could not
/// bind its port because another process holds it.
pub fn gateway_port_held(health: &Value) -> Option<String> {
    let error = health["components"]["gateway"]["last_error"].as_str()?;
    let lower = error.to_ascii_lowercase();
    let in_use = lower.contains("address already in use")
        || lower.contains("only one usage of each socket address")
        || lower.contains("(os error 48)")
        || lower.contains("(os error 98)")
        || lower.contains("(os error 10048)");
    in_use.then(|| error.to_string())
}

/// Wait, within `budget` overall, until the dashboard's gateway is ready.
///
/// For the daemon this app launched, `core` is its verified RPC connection,
/// and the dashboard address must prove it is that core's own gateway (see
/// [`crate::possession`]). An HTTP answer alone proves nothing about who
/// serves it: any program can hold the port and copy any body, and a gateway
/// that lost the port to one is reported as such. Without a core (an older
/// kernel), a successful `/health` is all there is to go on.
pub async fn await_gateway(
    gateway_url: &str,
    core: Option<&RpcClient>,
    budget: Duration,
) -> Result<(), StartupFailure> {
    let client = GatewayClient::new(gateway_url, None);
    let mut progress = GatewayProgress::default();
    let wait = async {
        loop {
            let ready = match core {
                Some(core) => launched_gateway_ready(core, gateway_url, &mut progress).await?,
                None => client.get_health().await.unwrap_or(false),
            };
            if ready {
                return Ok(());
            }
            tokio::time::sleep(GATEWAY_POLL).await;
        }
    };
    let outcome = tokio::time::timeout(budget, wait).await;
    outcome.unwrap_or_else(|_| {
        let reported = progress
            .note
            .map(|note| format!(" Its gateway: {note}."))
            .unwrap_or_default();
        Err(StartupFailure::Timeout(format!(
            "ZeroClaw did not finish starting within {} seconds.{reported} Its log is in the ZeroClaw config directory under logs/zeroclaw-desktop-daemon.log.",
            budget.as_secs()
        )))
    })
}

/// What the gateway wait has seen so far.
#[derive(Default)]
struct GatewayProgress {
    /// The latest reason the gateway was not ready, for the timeout message.
    note: Option<String>,
    /// Answers in a row on the dashboard address without the core's proof.
    mismatches: u32,
}

/// One check of the launched daemon's gateway: `Ok(true)` once the dashboard
/// address proves it is the core's own gateway.
///
/// The proof is the readiness, and it alone admits. It is checked against a
/// challenge the core issues over the verified RPC connection for the
/// listener it vouches for at exactly the dashboard's address, which the
/// supervisor pins: the core's own gateway, or a separate gateway that
/// registered its listener there. What the core's own gateway reports only
/// explains a failed proof (see [`own_gateway`]), as a process ID the address
/// reports over HTTP does: neither ever decides that the address is trusted.
async fn launched_gateway_ready(
    core: &RpcClient,
    gateway_url: &str,
    progress: &mut GatewayProgress,
) -> Result<bool, StartupFailure> {
    let dashboard = dashboard_addr(gateway_url).ok_or_else(|| {
        StartupFailure::Incompatible(format!(
            "The dashboard address {gateway_url} is not an IP address and port this app can check."
        ))
    })?;
    match crate::possession::prove_gateway(core, dashboard).await {
        Ok(_proven) => Ok(true),
        Err(ProofFailure::CoreUnavailable(error)) => Err(core_stopped(&error)),
        Err(failure @ ProofFailure::Unsupported(_)) => Err(StartupFailure::Incompatible(format!(
            "The dashboard's address cannot be proven: {failure}."
        ))),
        Err(ProofFailure::ElsewhereBound(bound)) => Err(StartupFailure::PortHeld(format!(
            "The ZeroClaw core's gateway is listening on {bound}, not on the dashboard's address {dashboard}. Another program may hold that address; close it and reopen ZeroClaw."
        ))),
        Err(ProofFailure::NotBound) => {
            let gateway = own_gateway(core, gateway_url, dashboard).await?;
            progress.mismatches = 0;
            progress.note = gateway["last_error"]
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    (gateway["status"].as_str() == Some("ok")).then(|| {
                        "it is running but has not reported the address it bound".to_string()
                    })
                });
            Ok(false)
        }
        Err(failure @ ProofFailure::Unreachable(_)) => {
            own_gateway(core, gateway_url, dashboard).await?;
            progress.mismatches = 0;
            progress.note = Some(failure.to_string());
            Ok(false)
        }
        Err(failure @ ProofFailure::Mismatch { .. }) => {
            own_gateway(core, gateway_url, dashboard).await?;
            progress.mismatches += 1;
            progress.note = Some(failure.to_string());
            if progress.mismatches < PROOF_MISMATCHES_HELD {
                return Ok(false);
            }
            let core_pid = core.handshake().server_pid;
            Err(StartupFailure::PortHeld(format!(
                "The dashboard's address {gateway_url} is not served by the ZeroClaw core this app started (process {core_pid}): {failure}. Close the other program and reopen ZeroClaw."
            )))
        }
    }
}

/// The core's report on its own gateway, asked only once the dashboard
/// address has not proven itself, to explain why. A port another program
/// holds (the gateway's bind error), or the gateway bound to another address,
/// ends the wait as `port_held`; otherwise the gateway's report, for the
/// note. It never admits an address.
async fn own_gateway(
    core: &RpcClient,
    gateway_url: &str,
    dashboard: SocketAddr,
) -> Result<Value, StartupFailure> {
    // The daemon rejects `"params": null`; an empty object is the no-argument
    // form it accepts.
    let health = core
        .request(Method::Health, Value::Object(serde_json::Map::new()))
        .await
        .map_err(|error| core_stopped(&error.to_string()))?;
    if let Some(error) = gateway_port_held(&health) {
        return Err(StartupFailure::PortHeld(format!(
            "Another program is using the dashboard's address {gateway_url}: {error}. Close it and reopen ZeroClaw."
        )));
    }
    let gateway = &health["components"]["gateway"];
    // The core's own gateway, when it has one, reports where it is bound. On
    // another address it cannot be the dashboard's, and whatever answers
    // there is another program.
    if let Some(bound) = gateway["bound_addr"].as_str()
        && bound.parse::<SocketAddr>().ok() != Some(dashboard)
    {
        return Err(StartupFailure::PortHeld(format!(
            "The ZeroClaw core's gateway is listening on {bound}, not on the dashboard's address {dashboard}. Another program may hold that address; close it and reopen ZeroClaw."
        )));
    }
    Ok(gateway.clone())
}

fn core_stopped(error: &str) -> StartupFailure {
    StartupFailure::CoreUnavailable(format!(
        "The ZeroClaw core stopped answering while it started: {error}. Its log is in the ZeroClaw config directory under logs/zeroclaw-desktop-daemon.log."
    ))
}

/// The dashboard's socket address, from its URL: an IP literal and a port.
pub fn dashboard_addr(gateway_url: &str) -> Option<SocketAddr> {
    let url = reqwest::Url::parse(gateway_url).ok()?;
    let host = url.host_str()?;
    let ip: IpAddr = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()?;
    Some(SocketAddr::new(ip, url.port_or_known_default()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(unix)]
    use std::time::Instant;

    fn handshake(protocol: u64, version: &str, pid: u32) -> CoreHandshake {
        CoreHandshake {
            protocol_version: protocol,
            server_version: version.to_string(),
            server_pid: pid,
        }
    }

    #[test]
    fn a_matching_bundled_core_passes() {
        assert_eq!(
            check_handshake(
                &handshake(RPC_PROTOCOL_VERSION, "0.9.0", 7),
                Some(7),
                true,
                "0.9.0"
            ),
            Ok(())
        );
    }

    #[test]
    fn another_protocol_is_incompatible() {
        let failure = check_handshake(&handshake(2, "0.9.0", 7), Some(7), true, "0.9.0")
            .expect_err("protocol 2 is not spoken");
        assert_eq!(failure.kind(), "incompatible");
        assert!(failure.is_final());
    }

    #[test]
    fn a_bundled_core_of_another_version_is_incompatible() {
        let failure = check_handshake(
            &handshake(RPC_PROTOCOL_VERSION, "0.8.5", 7),
            Some(7),
            true,
            "0.9.0",
        )
        .expect_err("a bundled pair must match exactly");
        assert_eq!(failure.kind(), "incompatible");
        assert!(failure.message().contains("0.8.5") && failure.message().contains("0.9.0"));
    }

    #[test]
    fn a_separately_installed_core_only_needs_the_protocol() {
        assert_eq!(
            check_handshake(
                &handshake(RPC_PROTOCOL_VERSION, "0.8.5", 7),
                Some(7),
                false,
                "0.9.0"
            ),
            Ok(())
        );
    }

    #[test]
    fn another_core_on_the_endpoint_is_reported() {
        let failure = check_handshake(
            &handshake(RPC_PROTOCOL_VERSION, "0.9.0", 99),
            Some(7),
            true,
            "0.9.0",
        )
        .expect_err("a different process answered");
        assert_eq!(failure.kind(), "endpoint_held");
        assert!(failure.message().contains("99"));
    }

    #[test]
    fn a_gateway_that_lost_its_port_is_recognised_on_every_platform() {
        for error in [
            "Failed to bind 127.0.0.1:42617: Address already in use (os error 48)",
            "Failed to bind 127.0.0.1:42617: Address already in use (os error 98)",
            "Failed to bind 127.0.0.1:42617: Only one usage of each socket address (protocol/network address/port) is normally permitted. (os error 10048)",
        ] {
            let health = json!({ "components": { "gateway": { "last_error": error } } });
            assert_eq!(gateway_port_held(&health).as_deref(), Some(error));
        }
        let unrelated = json!({ "components": { "gateway": { "last_error": "TLS key missing" } } });
        assert_eq!(gateway_port_held(&unrelated), None);
        assert_eq!(gateway_port_held(&json!({ "components": {} })), None);
    }

    #[test]
    fn only_a_timeout_leaves_the_launched_daemon_running() {
        assert!(!StartupFailure::Timeout(String::new()).is_final());
        assert!(StartupFailure::PortHeld(String::new()).is_final());
        assert!(StartupFailure::EndpointHeld(String::new()).is_final());
        assert!(StartupFailure::CoreUnavailable(String::new()).is_final());
    }

    /// A core listening on a private socket answers `initialize`; the app
    /// verifies the endpoint's account, completes the handshake and checks
    /// the version pair.
    #[cfg(unix)]
    #[tokio::test]
    async fn verify_core_completes_against_a_same_account_endpoint() {
        let dir = PrivateDir::new("same");
        let endpoint = dir.0.join("d.sock");
        let pid = serve_initialize(&endpoint, env!("CARGO_PKG_VERSION"));
        verify_core(&endpoint, Some(pid), true)
            .await
            .expect("a matching core on a private endpoint verifies");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verify_core_refuses_a_bundled_core_of_another_version() {
        let dir = PrivateDir::new("skew");
        let endpoint = dir.0.join("d.sock");
        let pid = serve_initialize(&endpoint, "0.0.1");
        let failure = verify_core(&endpoint, Some(pid), true)
            .await
            .expect_err("the bundled pair differs");
        assert_eq!(failure.kind(), "incompatible");
    }

    /// The core reports that its gateway lost the dashboard port. An HTTP
    /// 200 from some other listener on the address must not override that.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_foreign_http_answer_does_not_override_the_core_s_port_failure() {
        let core = FakeCore::start(
            "held",
            gateway_health("error", Some(PORT_HELD), None),
            false,
        )
        .await;
        let control = await_gateway(&fake_http(503, None), Some(&core.client), SHORT).await;
        assert!(
            matches!(control, Err(StartupFailure::PortHeld(_))),
            "{control:?}"
        );

        let foreign = fake_http(200, Some(json!({ "status": "ok" })));
        let outcome = await_gateway(&foreign, Some(&core.client), SHORT).await;
        assert!(
            matches!(outcome, Err(StartupFailure::PortHeld(_))),
            "{outcome:?}"
        );
        assert_eq!(
            core.health_calls(),
            2,
            "the core is asked before HTTP is trusted"
        );
    }

    /// The core reports its gateway bound to the dashboard address, but that
    /// address answers as another process: the PID mismatch still refuses.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_address_answering_as_another_process_is_refused() {
        let other_pid = std::process::id() + 1;
        let gateway = fake_http(200, Some(json!({ "runtime": { "pid": other_pid } })));
        let core = FakeCore::start(
            "other",
            gateway_health("ok", None, Some(&bound_addr_of(&gateway))),
            false,
        )
        .await;
        match await_gateway(&gateway, Some(&core.client), SHORT).await {
            Err(StartupFailure::PortHeld(message)) => {
                assert!(message.contains(&other_pid.to_string()), "{message}");
            }
            other => panic!("expected PortHeld, got {other:?}"),
        }
    }

    /// The dashboard address proves it holds the key of the listener the core
    /// vouches for: the gateway is ready.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_launched_gateway_is_ready_once_the_dashboard_address_proves_it() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let core = FakeCore::start(
            "agree",
            gateway_health("ok", None, Some(&bound_addr_of(&gateway.url))),
            false,
        )
        .await;
        await_gateway(&gateway.url, Some(&core.client), SHORT)
            .await
            .expect("the core's own gateway answers");
    }

    /// A separately installed core of this protocol from before the proof
    /// cannot vouch for its gateway. That is reported as a version skew, not
    /// as a stopped core, and no credential is sent anywhere.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_core_that_predates_the_proof_is_incompatible() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let core = FakeCore::start_predating_the_proof(
            "older",
            gateway_health("ok", None, Some(&bound_addr_of(&gateway.url))),
        )
        .await;
        match await_gateway(&gateway.url, Some(&core.client), SHORT).await {
            Err(failure @ StartupFailure::Incompatible(_)) => {
                assert!(failure.is_final());
                assert!(
                    failure.message().contains("gateway/possession-challenge"),
                    "{}",
                    failure.message()
                );
            }
            other => panic!("an older core must be reported incompatible: {other:?}"),
        }
        let (link, _dir) = core.into_link(&gateway.url);
        let refused = link
            .new_pairing_code()
            .await
            .expect_err("an older core mints no code");
        assert!(
            refused.contains("does not support pairing/new-code"),
            "{refused}"
        );
        assert!(
            gateway.seen().is_empty(),
            "nothing reached the dashboard address"
        );
    }

    /// A separate process on the dashboard address reports the core's real
    /// process ID, while the core vouches for a listener there (a snapshot of
    /// its own, released since). The address cannot answer the challenge, so
    /// it is refused: a process ID admits nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_correct_pid_impostor_on_the_dashboard_address_fails_the_proof() {
        let core_pid = std::process::id();
        let mut impostor = CopiedPidServer::start(core_pid);
        assert_ne!(impostor.pid(), core_pid, "the impostor is its own process");
        let dashboard = format!("http://127.0.0.1:{}", impostor.port);
        let core = FakeCore::start(
            "impostor",
            gateway_health("ok", None, Some(&bound_addr_of(&dashboard))),
            false,
        )
        .await;
        let outcome = await_gateway(&dashboard, Some(&core.client), SHORT).await;
        impostor.stop();
        match outcome {
            Err(StartupFailure::PortHeld(message)) => {
                assert!(
                    message.contains("without the ZeroClaw core's proof"),
                    "{message}"
                );
                assert!(message.contains(&core_pid.to_string()), "{message}");
            }
            other => panic!("a copied process ID must not pass the proof: {other:?}"),
        }
    }

    /// A gateway in a separate process, with its own process ID, that holds
    /// the key of the listener the core vouches for is ready: readiness
    /// follows the listener's key, never the core's process ID.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_separate_gateway_process_holding_the_vouched_key_is_ready() {
        let mut gateway = CopiedPidServer::start_with(4_000_000, None, Some(FAKE_KEY), None);
        assert_ne!(gateway.pid(), std::process::id());
        let dashboard = format!("http://127.0.0.1:{}", gateway.port);
        let core = FakeCore::start(
            "separate",
            gateway_health("ok", None, Some(&bound_addr_of(&dashboard))),
            false,
        )
        .await;
        let outcome = await_gateway(&dashboard, Some(&core.client), SHORT).await;
        gateway.stop();
        outcome.expect("the vouched listener's key is the proof");
    }

    /// A credential goes out only on the connection that just proved it is
    /// the core's gateway: the proof request and the credential share it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_credential_travels_on_the_connection_that_proved_itself() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let core = FakeCore::start(
            "carry",
            gateway_health("ok", None, Some(&bound_addr_of(&gateway.url))),
            false,
        )
        .await;
        let (link, _dir) = core.into_link(&gateway.url);
        let client = GatewayClient::new(&gateway.url, Some("zc_secret"))
            .with_core(Some(std::sync::Arc::new(link)));
        assert!(client.validate_token().await.unwrap());

        let seen = gateway.seen();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[0].1.starts_with("get /health?challenge="), "{seen:?}");
        assert!(seen[1].1.starts_with("get /api/status"), "{seen:?}");
        assert!(
            seen[1].1.contains("authorization: bearer zc_secret"),
            "{seen:?}"
        );
        assert!(
            !seen[0].1.contains("authorization"),
            "the proof request carries no credential"
        );
        assert_eq!(
            seen[0].0, seen[1].0,
            "one connection for proof and credential"
        );
    }

    /// The core's listener proved itself, then stopped, and another process
    /// took its address while the core still names it (a challenge taken
    /// before the release). The next credential is not sent: the new holder
    /// cannot answer the proof, and it never sees the token.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_listener_replaced_after_its_proof_receives_no_credential() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let port = gateway.port;
        let core = FakeCore::start(
            "moved",
            gateway_health("ok", None, Some(&bound_addr_of(&gateway.url))),
            false,
        )
        .await;
        let url = gateway.url.clone();
        let (link, dir) = core.into_link(&url);
        let link = std::sync::Arc::new(link);
        link.prove()
            .await
            .expect("the core's listener proves itself");

        gateway.stop();
        let log = dir.0.join("impostor.log");
        let mut impostor =
            CopiedPidServer::start_with(std::process::id(), Some(port), None, Some(&log));
        assert_eq!(impostor.port, port, "the impostor took the address");
        let client = GatewayClient::new(&url, Some("zc_secret")).with_core(Some(link));
        let validated = client.validate_token().await.unwrap();
        let status = client.get_status().await;
        impostor.stop();

        assert!(!validated, "an unproven address validates nothing");
        let refused = status.expect_err("no credential without a proof");
        assert!(
            refused
                .to_string()
                .contains("refusing to send a credential"),
            "{refused}"
        );
        let received = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            received.contains("challenge="),
            "the impostor was challenged"
        );
        assert!(!received.contains("authorization"), "{received}");
        assert!(!received.contains("zc_secret"), "{received}");
    }

    /// The pairing code comes from the core over its socket, and the code goes
    /// to the gateway only on a proven connection.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_pairing_code_comes_from_the_core_and_pairs_on_a_proven_connection() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let core = FakeCore::start(
            "pair",
            gateway_health("ok", None, Some(&bound_addr_of(&gateway.url))),
            false,
        )
        .await;
        let (link, _dir) = core.into_link(&gateway.url);
        let link = std::sync::Arc::new(link);
        let code = link.new_pairing_code().await.expect("a code from the core");
        assert_eq!(code, FAKE_PAIRING_CODE);
        let token = GatewayClient::new(&gateway.url, None)
            .with_core(Some(link))
            .pair_with_code(&code)
            .await
            .expect("the code pairs");
        assert_eq!(token, "zc_paired");

        let seen = gateway.seen();
        assert!(seen[0].1.starts_with("get /health?challenge="), "{seen:?}");
        assert!(seen[1].1.starts_with("post /pair"), "{seen:?}");
        assert!(seen[1].1.contains("x-pairing-code: 246810"), "{seen:?}");
        assert_eq!(seen[0].0, seen[1].0);
    }

    /// The core's gateway is bound somewhere else (here IPv6 loopback on the
    /// same port), while a separate process listens on the dashboard address
    /// and reports the core's real process ID. Copying the ID gains nothing:
    /// only the address the core reports as bound admits.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_distinct_process_copying_the_core_pid_is_refused() {
        let core_pid = std::process::id();
        let mut impostor = CopiedPidServer::start(core_pid);
        assert_ne!(impostor.pid(), core_pid, "the impostor is its own process");
        let dashboard = format!("http://127.0.0.1:{}", impostor.port);
        let core = FakeCore::start(
            "copied",
            gateway_health("ok", None, Some(&format!("[::1]:{}", impostor.port))),
            false,
        )
        .await;

        let outcome = await_gateway(&dashboard, Some(&core.client), SHORT).await;
        impostor.stop();
        match outcome {
            Err(StartupFailure::PortHeld(message)) => {
                assert!(message.contains("[::1]"), "{message}");
            }
            other => panic!("the copied process ID must not admit the address: {other:?}"),
        }
    }

    /// Without a bound address from the core, an HTTP answer carrying the
    /// core's own process ID is still not readiness.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_core_pid_alone_never_admits_the_dashboard_address() {
        let gateway = fake_http(
            200,
            Some(json!({ "runtime": { "pid": std::process::id() } })),
        );
        let core = FakeCore::start("unbound", gateway_health("ok", None, None), false).await;
        match await_gateway(&gateway, Some(&core.client), Duration::from_millis(300)).await {
            Err(StartupFailure::Timeout(message)) => {
                assert!(
                    message.contains("has not reported the address it bound"),
                    "{message}"
                );
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    /// An HTTP answer, even from the right process, is not readiness while
    /// the core still reports its gateway starting or failing; the timeout
    /// names the gateway's last error.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_gateway_the_core_does_not_report_bound_is_not_ready() {
        let core = FakeCore::start(
            "start",
            gateway_health("error", Some("TLS key missing"), None),
            false,
        )
        .await;
        let gateway = fake_http(
            200,
            Some(json!({ "runtime": { "pid": std::process::id() } })),
        );
        match await_gateway(&gateway, Some(&core.client), Duration::from_millis(300)).await {
            Err(StartupFailure::Timeout(message)) => {
                assert!(message.contains("TLS key missing"), "{message}");
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    /// The deadline bounds the whole wait, including a core request in
    /// flight that the core never answers.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_gateway_deadline_bounds_an_in_flight_core_request() {
        let core = FakeCore::start("stall", gateway_health("ok", None, None), true).await;
        let gateway = fake_http(503, None);
        let started = Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            await_gateway(&gateway, Some(&core.client), Duration::from_millis(30)),
        )
        .await
        .expect("the gateway deadline must end the wait, not the core request timeout");
        assert!(
            matches!(outcome, Err(StartupFailure::Timeout(_))),
            "{outcome:?}"
        );
        assert_eq!(
            core.health_calls(),
            1,
            "the core received the request it never answers"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    /// A gateway in another process that registered its listener at the
    /// dashboard address, and answers the challenge for it, is ready whatever
    /// the core's own gateway reports: here that it lost the port, or that it
    /// listens somewhere else. The proof decides; the core's own report only
    /// explains a failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_proven_registered_listener_is_ready_whatever_the_core_s_own_gateway_reports() {
        for (label, own) in [
            ("lost", gateway_health("error", Some(PORT_HELD), None)),
            ("elsewhere", gateway_health("ok", None, Some("127.0.0.1:1"))),
        ] {
            let mut gateway = CopiedPidServer::start_with(4_000_000, None, Some(FAKE_KEY), None);
            let dashboard = format!("http://127.0.0.1:{}", gateway.port);
            let core = FakeCore::start_with(
                label,
                own,
                CoreOptions {
                    registered: Some(bound_addr_of(&dashboard)),
                    ..CoreOptions::default()
                },
            )
            .await;
            let outcome = await_gateway(&dashboard, Some(&core.client), SHORT).await;
            gateway.stop();
            if let Err(failure) = outcome {
                panic!("{label}: a registered listener that proves itself is ready: {failure:?}");
            }
        }
    }

    /// A challenge the core issued while its listener held the dashboard
    /// address, delivered only after that listener let go and another program
    /// took the address, admits nothing: the new holder cannot answer it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_challenge_delivered_after_the_listener_moved_admits_nothing() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let (port, url) = (gateway.port, gateway.url.clone());
        let (held, arrived) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let core = FakeCore::start_with(
            "late",
            gateway_health("ok", None, Some(&bound_addr_of(&url))),
            CoreOptions {
                hold: Some(("gateway/possession-challenge", held, released)),
                ..CoreOptions::default()
            },
        )
        .await;
        let waiting =
            async move { await_gateway(&url, Some(&core.client), Duration::from_secs(3)).await };
        let replace_listener = async move {
            tokio::task::spawn_blocking(move || arrived.recv_timeout(Duration::from_secs(5)))
                .await
                .unwrap()
                .expect("the challenge is held");

            gateway.stop();
            let impostor = CopiedPidServer::start_with(std::process::id(), Some(port), None, None);
            assert_eq!(impostor.port, port, "the impostor took the address");
            release.send(()).unwrap();
            impostor
        };
        let (outcome, mut impostor) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(waiting, replace_listener)
        })
        .await
        .expect("the wait ends");
        impostor.stop();
        assert!(outcome.is_err(), "{outcome:?}");
    }

    /// The app's state after a ready startup with the core this test serves.
    #[cfg(unix)]
    async fn ready_state(
        url: &str,
        link: crate::possession::CoreLink,
        token: &str,
    ) -> crate::state::SharedState {
        let state = crate::state::shared_state();
        {
            let mut s = state.write().await;
            s.gateway_url = url.to_string();
            s.startup = crate::state::Startup::Ready;
            s.core = Some(std::sync::Arc::new(link));
            s.token = Some(token.to_string());
        }
        state
    }

    /// The dashboard opens only on an address proven right then to be the
    /// core's gateway, whatever the pairing check found. A listener that
    /// proved itself, replaced by a program without its key that says no
    /// pairing is needed, gets no dashboard window though a token is saved,
    /// and never sees the token.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_listener_saying_no_pairing_is_needed_gets_no_dashboard() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let (port, url) = (gateway.port, gateway.url.clone());
        let core = FakeCore::start(
            "nopair",
            gateway_health("ok", None, Some(&bound_addr_of(&url))),
            false,
        )
        .await;
        await_gateway(&url, Some(&core.client), SHORT)
            .await
            .expect("the core's listener proves itself");
        let (link, dir) = core.into_link(&url);
        let state = ready_state(&url, link, "zc_saved").await;

        gateway.stop();
        let log = dir.0.join("impostor.log");
        let mut impostor =
            CopiedPidServer::start_with(std::process::id(), Some(port), None, Some(&log));
        assert_eq!(impostor.port, port, "the impostor took the address");
        let mut windows = 0;
        let outcome = crate::open_admitted_dashboard(&state, |_| {
            windows += 1;
            Ok(())
        })
        .await;
        impostor.stop();

        let refused = outcome.expect_err("an unproven address gets no dashboard");
        assert_eq!(windows, 0, "no dashboard window was built");
        assert!(
            refused.contains("not the ZeroClaw core's gateway"),
            "{refused}"
        );
        let received = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!received.contains("zc_saved"), "{received}");
    }

    /// On an address that proves itself, the dashboard window is built once,
    /// with the saved token.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_dashboard_opens_on_a_proven_address_with_the_saved_token() {
        let gateway = ProvingHttp::start(Some(FAKE_KEY));
        let url = gateway.url.clone();
        let core = FakeCore::start(
            "open",
            gateway_health("ok", None, Some(&bound_addr_of(&url))),
            false,
        )
        .await;
        let (link, _dir) = core.into_link(&url);
        let state = ready_state(&url, link, "zc_saved").await;
        let mut opened = Vec::new();
        crate::open_admitted_dashboard(&state, |opening| {
            opened.push(opening.token);
            Ok(())
        })
        .await
        .expect("a proven address opens");
        assert_eq!(opened, vec![Some("zc_saved".to_string())]);
    }

    #[cfg(unix)]
    const SHORT: Duration = Duration::from_secs(10);
    /// The fake core's possession key, and the proof any holder of it gives.
    #[cfg(unix)]
    const FAKE_KEY: &str = "fake-gateway-key";
    #[cfg(unix)]
    const FAKE_PAIRING_CODE: &str = "246810";

    #[cfg(unix)]
    fn proof_of(key: &str, nonce: &str) -> String {
        format!("proof:{key}:{nonce}")
    }
    #[cfg(unix)]
    const PORT_HELD: &str = "Failed to bind 127.0.0.1:42617: Address already in use (os error 48)";

    #[cfg(unix)]
    fn gateway_health(status: &str, last_error: Option<&str>, bound_addr: Option<&str>) -> Value {
        let mut gateway = json!({ "status": status, "last_error": last_error });
        if let Some(bound_addr) = bound_addr {
            gateway["bound_addr"] = json!(bound_addr);
        }
        json!({ "components": { "gateway": gateway } })
    }

    /// The `host:port` a `fake_http` URL listens on.
    #[cfg(unix)]
    fn bound_addr_of(url: &str) -> String {
        url.trim_start_matches("http://").to_string()
    }

    #[cfg(unix)]
    const COPIED_PID_ENV: &str = "ZEROCLAW_DESKTOP_TEST_COPIED_PID";
    #[cfg(unix)]
    const HELPER_PORT_ENV: &str = "ZEROCLAW_DESKTOP_TEST_HELPER_PORT";
    #[cfg(unix)]
    const HELPER_KEY_ENV: &str = "ZEROCLAW_DESKTOP_TEST_HELPER_KEY";
    #[cfg(unix)]
    const HELPER_LOG_ENV: &str = "ZEROCLAW_DESKTOP_TEST_HELPER_LOG";

    /// A separate process serving `/health` on a loopback port and reporting
    /// whatever process ID it is given: the test binary itself, re-run as
    /// [`copied_pid_http_helper`].
    #[cfg(unix)]
    struct CopiedPidServer {
        child: std::process::Child,
        port: u16,
    }

    #[cfg(unix)]
    impl CopiedPidServer {
        fn start(reported_pid: u32) -> Self {
            Self::start_with(reported_pid, None, None, None)
        }

        /// The helper process, on `port` when given, proving `key` when
        /// given, and appending each request head it reads to `log`.
        fn start_with(
            reported_pid: u32,
            port: Option<u16>,
            key: Option<&str>,
            log: Option<&Path>,
        ) -> Self {
            use std::io::BufRead;
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("test binary"));
            command
                .args([
                    "--exact",
                    "readiness::tests::copied_pid_http_helper",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(COPIED_PID_ENV, reported_pid.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            if let Some(port) = port {
                command.env(HELPER_PORT_ENV, port.to_string());
            }
            if let Some(key) = key {
                command.env(HELPER_KEY_ENV, key);
            }
            if let Some(log) = log {
                command.env(HELPER_LOG_ENV, log);
            }
            let mut child = command.spawn().expect("start the helper process");
            let stdout = child.stdout.take().expect("impostor stdout");
            let port = std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
                // libtest's own `test … ...` prefix can share the line.
                .find_map(|line| {
                    line.rsplit_once("PORT ")
                        .and_then(|(_, port)| port.trim().parse().ok())
                })
                .expect("the impostor reports its port");
            Self { child, port }
        }

        fn pid(&self) -> u32 {
            self.child.id()
        }

        fn stop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(unix)]
    impl Drop for CopiedPidServer {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// The `challenge` query value in a request head, if any.
    #[cfg(unix)]
    fn challenge_of(head: &str) -> Option<String> {
        let target = head.split_whitespace().nth(1)?;
        let query = target.split_once('?')?.1;
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix("challenge="))
            .map(str::to_string)
    }

    /// Drives the app's startup and credential handoff against a real kernel:
    /// `ZEROCLAW_DESKTOP_E2E_ENDPOINT` and `_PID` from its `READY` frame,
    /// `_URL` its dashboard address. Run by hand; prints what it saw.
    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "needs a real kernel started with --rpc-readiness"]
    async fn real_kernel_possession_flow() {
        let (Some(endpoint), Some(pid), Some(url)) = (
            std::env::var_os("ZEROCLAW_DESKTOP_E2E_ENDPOINT"),
            std::env::var("ZEROCLAW_DESKTOP_E2E_PID").ok(),
            std::env::var("ZEROCLAW_DESKTOP_E2E_URL").ok(),
        ) else {
            return;
        };
        let endpoint = std::path::PathBuf::from(endpoint);
        let started = Instant::now();
        let core = verify_core(&endpoint, pid.parse().ok(), false)
            .await
            .expect("the real core verifies");
        await_gateway(&url, Some(&core), Duration::from_secs(60))
            .await
            .expect("the real gateway proves itself");
        println!("E2E ready in {:?}", started.elapsed());
        let link = std::sync::Arc::new(crate::possession::CoreLink::new(
            core,
            endpoint,
            false,
            dashboard_addr(&url).unwrap(),
        ));
        let code = link.new_pairing_code().await.expect("a code over RPC");
        println!("E2E pairing code over RPC: {} characters", code.len());
        let token = GatewayClient::new(&url, None)
            .with_core(Some(std::sync::Arc::clone(&link)))
            .pair_with_code(&code)
            .await
            .expect("the code pairs on a proven connection");
        let valid = GatewayClient::new(&url, Some(&token))
            .with_core(Some(std::sync::Arc::clone(&link)))
            .validate_token()
            .await
            .unwrap();
        println!("E2E token issued and valid on a proven connection: {valid}");
        assert!(valid);
        let status = GatewayClient::new(&url, Some(&token))
            .with_core(Some(link))
            .get_status()
            .await
            .expect("status over a proven connection");
        println!(
            "E2E /api/status keys: {:?}",
            status.as_object().map(|o| o.len())
        );
    }

    /// The helper process for the tests that need a distinct HTTP process.
    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess helper for a_distinct_process_copying_the_core_pid_is_refused"]
    fn copied_pid_http_helper() {
        use std::io::{BufRead, BufReader, Write};
        let Some(pid) = std::env::var_os(COPIED_PID_ENV) else {
            return;
        };
        let pid: u32 = pid.to_string_lossy().parse().unwrap();
        let key = std::env::var(HELPER_KEY_ENV).ok();
        let log = std::env::var_os(HELPER_LOG_ENV);
        let port: u16 = std::env::var(HELPER_PORT_ENV)
            .ok()
            .map_or(0, |port| port.parse().unwrap());
        // The address may still be closing in the process that held it.
        let bind_deadline = Instant::now() + Duration::from_secs(5);
        let listener = loop {
            match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => break listener,
                Err(error) if Instant::now() > bind_deadline => panic!("bind loopback: {error}"),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        println!("PORT {}", listener.local_addr().unwrap().port());
        std::io::stdout().flush().unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            let _ = stream.set_nonblocking(false);
            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                head.push_str(&line);
                line.clear();
            }
            if let Some(log) = &log {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log)
                    .unwrap();
                let _ = file.write_all(head.to_ascii_lowercase().as_bytes());
            }
            let mut body =
                json!({ "status": "ok", "require_pairing": false, "runtime": { "pid": pid } });
            if let (Some(key), Some(nonce)) = (key.as_deref(), challenge_of(&head)) {
                body["challenge_proof"] = json!(proof_of(key, &nonce));
            }
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(response.as_bytes());
        }
        std::process::exit(0);
    }

    /// How a [`FakeCore`] differs from a plain current core.
    #[cfg(unix)]
    #[derive(Default)]
    struct CoreOptions {
        /// Never answer `health`.
        stall: bool,
        /// Answer neither the challenge nor `pairing/new-code`, like a core
        /// from before the possession proof.
        predates_proof: bool,
        /// The address of a gateway in another process that registered its
        /// listener with the core, which the core vouches for with the fake
        /// key after its own listener.
        registered: Option<String>,
        /// Hold the first answer to this method: tell the first channel it
        /// is held, and answer once the second one fires.
        hold: Option<(
            &'static str,
            std::sync::mpsc::SyncSender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    }

    /// A launched core on a private socket, already verified: it answers
    /// `initialize` as this process, then every `health` with a fixed report,
    /// or never answers `health` when `stall` is set.
    #[cfg(unix)]
    struct FakeCore {
        dir: PrivateDir,
        endpoint: std::path::PathBuf,
        client: RpcClient,
        health_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[cfg(unix)]
    impl FakeCore {
        async fn start(label: &str, health: Value, stall: bool) -> Self {
            Self::start_with(
                label,
                health,
                CoreOptions {
                    stall,
                    ..CoreOptions::default()
                },
            )
            .await
        }

        /// A core of this protocol from before the possession proof: it
        /// answers neither the challenge nor `pairing/new-code`.
        async fn start_predating_the_proof(label: &str, health: Value) -> Self {
            Self::start_with(
                label,
                health,
                CoreOptions {
                    predates_proof: true,
                    ..CoreOptions::default()
                },
            )
            .await
        }

        async fn start_with(label: &str, health: Value, options: CoreOptions) -> Self {
            let CoreOptions {
                stall,
                predates_proof,
                registered,
                mut hold,
            } = options;
            use std::io::{BufRead, BufReader, Write};
            let dir = PrivateDir::new(label);
            let endpoint = dir.0.join("d.sock");
            let listener =
                std::os::unix::net::UnixListener::bind(&endpoint).expect("bind endpoint");
            let pid = std::process::id();
            let health_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = std::sync::Arc::clone(&health_calls);
            let next_nonce = std::sync::atomic::AtomicUsize::new(0);
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                let mut write = stream.try_clone().expect("clone stream");
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    let request: Value = serde_json::from_str(&line).expect("json request");
                    // Like the daemon's dispatcher, refuse null params.
                    if request["params"].is_null() {
                        let refusal = json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "error": {
                                "code": -32600,
                                "message": "Invalid request: params must be an object or array when present"
                            }
                        });
                        if write.write_all(format!("{refusal}\n").as_bytes()).is_err() {
                            break;
                        }
                        continue;
                    }
                    if predates_proof
                        && matches!(
                            request["method"].as_str(),
                            Some("gateway/possession-challenge" | "pairing/new-code")
                        )
                    {
                        let refusal = json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "error": { "code": error_codes::METHOD_NOT_FOUND, "message": "Method not found" }
                        });
                        if write.write_all(format!("{refusal}\n").as_bytes()).is_err() {
                            break;
                        }
                        continue;
                    }
                    let result = match request["method"].as_str() {
                        Some("initialize") => json!({
                            "protocol_version": RPC_PROTOCOL_VERSION,
                            "server_version": env!("CARGO_PKG_VERSION"),
                            "server_pid": pid,
                        }),
                        Some("health") => {
                            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if stall {
                                continue;
                            }
                            health.clone()
                        }
                        // Like the core: a challenge for the listener it
                        // vouches for at the asked address, its own (the one
                        // its health report names) first, then a registered
                        // one, proved with the fake key.
                        Some("gateway/possession-challenge") => {
                            let asked = request["params"]["addr"].as_str();
                            let own = health["components"]["gateway"]["bound_addr"].as_str();
                            let bound = [own, registered.as_deref()]
                                .into_iter()
                                .flatten()
                                .find(|bound| asked == Some(*bound));
                            if let Some(bound) = bound {
                                let nonce = format!(
                                    "nonce-{}",
                                    next_nonce.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                );
                                json!({
                                    "bound_addr": bound,
                                    "proof": proof_of(FAKE_KEY, &nonce),
                                    "nonce": nonce,
                                })
                            } else {
                                json!({ "bound_addr": null })
                            }
                        }
                        Some("pairing/new-code") => json!({
                            "success": true,
                            "pairing_required": true,
                            "pairing_code": FAKE_PAIRING_CODE,
                        }),
                        _ => json!({}),
                    };
                    if hold
                        .as_ref()
                        .is_some_and(|(method, _, _)| request["method"].as_str() == Some(*method))
                        && let Some((_, arrived, release)) = hold.take()
                    {
                        let _ = arrived.send(());
                        let _ = release.recv();
                    }
                    let answer = json!({ "jsonrpc": "2.0", "id": request["id"], "result": result });
                    if write.write_all(format!("{answer}\n").as_bytes()).is_err() {
                        break;
                    }
                }
            });
            let client = verify_core(&endpoint, Some(pid), true)
                .await
                .expect("a matching core on a private endpoint verifies");
            Self {
                dir,
                endpoint,
                client,
                health_calls,
            }
        }

        /// The app's link to this core, as kept after a ready startup.
        fn into_link(self, dashboard: &str) -> (crate::possession::CoreLink, PrivateDir) {
            let dashboard = dashboard_addr(dashboard).expect("an IP dashboard address");
            (
                crate::possession::CoreLink::new(self.client, self.endpoint, true, dashboard),
                self.dir,
            )
        }

        fn health_calls(&self) -> usize {
            self.health_calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// A gateway fake that keeps connections open: `/health?challenge=N`
    /// answers with the proof under `key` (none without one), `/pair` with a
    /// token, anything else `{}`. It records each request head, lower-cased,
    /// with the number of the connection it arrived on.
    #[cfg(unix)]
    struct ProvingHttp {
        url: String,
        port: u16,
        seen: std::sync::Arc<std::sync::Mutex<Vec<(usize, String)>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        accept: Option<std::thread::JoinHandle<()>>,
    }

    #[cfg(unix)]
    impl ProvingHttp {
        fn start(key: Option<&'static str>) -> Self {
            use std::io::{BufRead, BufReader, Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("local address").port();
            listener
                .set_nonblocking(true)
                .expect("non-blocking listener");
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let accept = {
                let seen = std::sync::Arc::clone(&seen);
                let stop = std::sync::Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut connection = 0;
                    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                        let Ok((stream, _)) = listener.accept() else {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        };
                        connection += 1;
                        let id = connection;
                        let seen = std::sync::Arc::clone(&seen);
                        std::thread::spawn(move || {
                            let _ = stream.set_nonblocking(false);
                            let mut reader = BufReader::new(stream);
                            loop {
                                let mut head = String::new();
                                let mut line = String::new();
                                let mut length = 0usize;
                                while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n"
                                {
                                    let lower = line.to_ascii_lowercase();
                                    if let Some(value) = lower.strip_prefix("content-length:") {
                                        length = value.trim().parse().unwrap_or(0);
                                    }
                                    head.push_str(&lower);
                                    line.clear();
                                }
                                if head.is_empty() {
                                    return;
                                }
                                let mut body = vec![0; length];
                                if reader.read_exact(&mut body).is_err() {
                                    return;
                                }
                                let answer = if head.starts_with("get /health") {
                                    let mut report =
                                        json!({ "status": "ok", "require_pairing": true });
                                    if let (Some(key), Some(nonce)) = (key, challenge_of(&head)) {
                                        report["challenge_proof"] = json!(proof_of(key, &nonce));
                                    }
                                    report
                                } else if head.starts_with("post /pair") {
                                    json!({ "token": "zc_paired" })
                                } else {
                                    json!({})
                                };
                                seen.lock().unwrap().push((id, head));
                                let answer = answer.to_string();
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{answer}",
                                    answer.len()
                                );
                                if reader.get_mut().write_all(response.as_bytes()).is_err() {
                                    return;
                                }
                            }
                        });
                    }
                })
            };
            Self {
                url: format!("http://127.0.0.1:{port}"),
                port,
                seen,
                stop,
                accept: Some(accept),
            }
        }

        fn seen(&self) -> Vec<(usize, String)> {
            self.seen.lock().unwrap().clone()
        }

        /// Close the listening socket: its address is free once this returns.
        fn stop(mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(accept) = self.accept.take() {
                let _ = accept.join();
            }
        }
    }

    #[cfg(unix)]
    impl Drop for ProvingHttp {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Answer HTTP on loopback for a few seconds, every request with `status`
    /// and `body`; returns the base URL.
    #[cfg(unix)]
    fn fake_http(status: u16, body: Option<Value>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                    line.clear();
                }
                let response = format!(
                    "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = reader.get_mut().write_all(response.as_bytes());
            }
        });
        url
    }

    /// A directory only this account can write, as the endpoint check requires.
    #[cfg(unix)]
    struct PrivateDir(std::path::PathBuf);

    #[cfg(unix)]
    impl PrivateDir {
        fn new(label: &str) -> Self {
            use std::os::unix::fs::DirBuilderExt;
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after the Unix epoch")
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("zc-ready-{label}-{}-{unique}", std::process::id()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .expect("create private directory");
            Self(dir)
        }
    }

    #[cfg(unix)]
    impl Drop for PrivateDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Answer one `initialize` on a socket bound at `endpoint`, reporting
    /// `version` and this process's ID; returns that ID.
    #[cfg(unix)]
    fn serve_initialize(endpoint: &Path, version: &str) -> u32 {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::os::unix::net::UnixListener::bind(endpoint).expect("bind endpoint");
        let pid = std::process::id();
        let version = version.to_string();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut write = stream.try_clone().expect("clone stream");
            let mut lines = BufReader::new(stream).lines();
            let request = lines.next().expect("a request").expect("read request");
            let request: Value = serde_json::from_str(&request).expect("json request");
            let answer = json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "protocol_version": RPC_PROTOCOL_VERSION,
                    "server_version": version,
                    "server_pid": pid,
                }
            });
            write
                .write_all(format!("{answer}\n").as_bytes())
                .expect("answer");
            // Keep the connection open while the client finishes.
            let _ = lines.next();
        });
        pid
    }
}
