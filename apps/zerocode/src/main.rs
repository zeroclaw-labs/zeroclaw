// `apps/zerocode` is a standalone TUI client, not daemon-path code.
// It speaks JSON-RPC to whatever ZeroClaw daemon is at the configured
// address; the daemon owns attribution, the TUI owns its session id.
// Bare `tokio::spawn` is the right primitive here — the workspace-wide
// `zeroclaw_spawn::spawn!` rule is daemon-path only (see `clippy.toml`'s
// commentary, which records this crate as the sole exemption).
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use clap::Parser;

mod acp;
mod app;
mod attachment;
mod chat;
mod client;
mod client_crypto;
mod clipboard;
mod color_depth;
mod config;
mod config_manager;
mod dashboard;
mod diff;
mod display_width;
mod doctor;
mod editor;
mod enroll;
mod file_explorer;
mod help;
mod i18n;
mod input_bar;
mod jsonrpc;
mod keymap;
mod logs;
mod mouse;
mod osc_status;
mod quickstart_pane;
mod relay_proto;
mod sop_pane;
mod terminal_backend;
#[cfg(test)]
mod test_support;
mod text_navigation;
mod text_selection;
mod theme;
mod todo_tracker;
mod turn_status;
mod widgets;
mod wire;
mod zerocode_pane;

const DAEMON_CONNECT_INTERVAL: Duration = Duration::from_millis(50);
const SPAWNED_DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_STDERR_LIMIT: usize = 8 * 1024;

/// Set to `true` once the alternate screen is active so signal/panic
/// handlers know they need to restore the terminal before exiting.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
struct TerminationSignals {
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
    quit: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl TerminationSignals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
            hangup: signal(SignalKind::hangup())?,
            quit: signal(SignalKind::quit())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.terminate.recv() => {}
            _ = self.interrupt.recv() => {}
            _ = self.hangup.recv() => {}
            _ = self.quit.recv() => {}
        }
    }
}

#[derive(Parser)]
#[command(
    name = "zerocode",
    about = "Interactive TUI config manager for ZeroClaw",
    version,
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\n\nThis version must exactly match the running zeroclaw daemon. ",
        "The TUI and daemon share a wire protocol with no cross-version ",
        "compatibility guarantee; mismatched versions may fail to connect ",
        "or behave unpredictably."
    )
)]
struct Cli {
    /// Path to the ZeroClaw config directory
    #[arg(long)]
    config_dir: Option<PathBuf>,

    /// Start in chat mode with this agent alias.
    /// If omitted, opens the config manager.
    #[arg(long, short = 'a')]
    agent: Option<String>,

    /// Connect to a remote daemon via WSS instead of the local Unix socket.
    /// Example: `--connect wss://host:9781`
    #[arg(long)]
    connect: Option<String>,

    /// Skip TLS certificate verification for WSS connections.
    /// Required for self-signed certificates. Only used with --connect.
    #[arg(long)]
    tls_skip_verify: bool,

    /// PEM CA certificate to verify the daemon (mutual TLS). Only used with --connect.
    #[arg(long)]
    tls_ca_cert: Option<String>,

    /// PEM client certificate to present to the daemon (mutual TLS).
    #[arg(long, requires = "tls_client_key")]
    tls_client_cert: Option<String>,

    /// PEM client private key for --tls-client-cert.
    #[arg(long, requires = "tls_client_cert")]
    tls_client_key: Option<String>,

    /// Reach the daemon through a nominated relay at this `host:port`
    /// (instead of connecting to --connect directly). Requires --relay-node.
    #[arg(long, requires = "relay_node")]
    relay: Option<String>,

    /// Node-id of the target daemon to request from the relay.
    #[arg(long, requires = "relay")]
    relay_node: Option<String>,

    /// PEM CA to trust for the relay's OWN (outer) certificate. Without it the
    /// built-in public roots are used (for a relay with a public-CA cert).
    #[arg(long)]
    relay_ca: Option<String>,

    /// Server name to expect on the relay's outer certificate. Defaults to the
    /// host portion of --relay.
    #[arg(long)]
    relay_host: Option<String>,

    /// Skip verification of the relay's outer certificate (self-signed dev only).
    #[arg(long)]
    relay_insecure: bool,

    /// Pin the relay's OUTER leaf certificate to this SHA-256 fingerprint (hex).
    /// Overrides --relay-ca / public roots. Usually delivered automatically at
    /// enrollment; pass it to pin a manually configured relay.
    #[arg(long)]
    relay_pin: Option<String>,

    /// Trust the relay's outer certificate on first use and remember its pin (for
    /// a self-hosted relay without enrollment). Opt-in; a known pin takes priority.
    #[arg(long)]
    relay_tofu: bool,

    /// PEM client certificate to present to the relay on the OUTER TLS layer
    /// (outer-mTLS variant), for a relay that requires outer client auth. Separate
    /// from --tls-client-cert (the inner mTLS to the daemon).
    #[arg(long, requires = "relay_client_key")]
    relay_client_cert: Option<String>,

    /// PEM private key for --relay-client-cert.
    #[arg(long, requires = "relay_client_cert")]
    relay_client_key: Option<String>,

    /// Enroll for a client certificate before connecting: prompt for the daemon
    /// pairing code, generate a key + CSR locally, fetch the signed cert, and
    /// cache it under <config-dir>/tls. The host defaults to --connect's host; the
    /// port defaults to the daemon's enrollment port (9782).
    #[arg(long)]
    enroll: bool,

    /// Host of the daemon enrollment endpoint (defaults to --connect's host).
    #[arg(long)]
    enroll_host: Option<String>,

    /// Port of the daemon enrollment endpoint (default 9782).
    #[arg(long)]
    enroll_port: Option<u16>,
}

/// Map an empty path string to `None`.
fn opt_path(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Which transport leg a live connection is actually using. Tracked so the
/// re-probe timer knows whether to attempt a migration back to the direct path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveLeg {
    Local,
    WssDirect,
    WssRelay,
}

/// A WSS route that may have BOTH a directly-reachable daemon address and a
/// relay. Connecting prefers the direct path and falls back to the relay tunnel;
/// once on the relay, a background timer re-probes the direct path and migrates
/// back when it returns.
pub(crate) struct WssRoute {
    /// The directly-reachable daemon address (`--connect` / `[wss].uri`). `None`
    /// in relay-only mode, where the daemon is reached solely through the relay.
    pub(crate) direct_url: Option<String>,
    /// Inner WSS URL used over a relay tunnel: the daemon's loopback SAN (the
    /// inner mTLS terminates at the daemon, the relay only forwards ciphertext).
    pub(crate) relay_inner_url: String,
    /// Relay coordinates, when a relay route is available.
    pub(crate) relay: Option<client::RelayDial>,
    /// TLS verification + mutual-TLS client identity, shared by both legs.
    pub(crate) tls: client::ClientTls,
    /// How many direct attempts before falling back to the relay (min 1).
    pub(crate) direct_attempts: u32,
    /// Per-attempt direct-connect timeout, in seconds (min 1).
    pub(crate) direct_timeout_secs: u64,
    /// Re-probe cadence while on the relay leg, in seconds (0 disables).
    pub(crate) reprobe_secs: u64,
}

impl WssRoute {
    /// The key under which an insecure (`skip_verify`) route is remembered: the
    /// direct address when present, else the inner relay URL.
    fn ack_key(&self) -> String {
        self.direct_url
            .clone()
            .unwrap_or_else(|| self.relay_inner_url.clone())
    }

    /// Connect preferring the direct path: try the direct address for a bounded
    /// number of attempts, then fall back to the relay tunnel. A relay-only route
    /// (no direct address) dials the relay immediately.
    async fn connect_preferred(
        &self,
        prev_id: Option<&str>,
        prev_sig: Option<&str>,
    ) -> anyhow::Result<(client::RpcClient, ActiveLeg)> {
        if let Some(url) = &self.direct_url {
            let mut last_err: Option<anyhow::Error> = None;
            for _ in 0..self.direct_attempts.max(1) {
                let fut = client::RpcClient::connect_wss_direct(url, prev_id, prev_sig, &self.tls);
                match tokio::time::timeout(
                    Duration::from_secs(self.direct_timeout_secs.max(1)),
                    fut,
                )
                .await
                {
                    Ok(Ok(client)) => return Ok((client, ActiveLeg::WssDirect)),
                    Ok(Err(e)) => last_err = Some(e),
                    Err(_) => {
                        last_err = Some(anyhow::Error::msg(format!(
                            "direct connect to {url} timed out after {}s",
                            self.direct_timeout_secs.max(1)
                        )))
                    }
                }
            }
            // Direct exhausted: fall back to the relay when one is available.
            if let Some(relay) = &self.relay {
                let client = client::RpcClient::connect_wss_via_relay(
                    &self.relay_inner_url,
                    prev_id,
                    prev_sig,
                    &self.tls,
                    relay,
                )
                .await?;
                return Ok((client, ActiveLeg::WssRelay));
            }
            return Err(last_err
                .unwrap_or_else(|| anyhow::Error::msg(format!("direct connect to {url} failed"))));
        }

        // Relay-only route.
        let relay = self.relay.as_ref().ok_or_else(|| {
            anyhow::Error::msg("WSS route has neither a direct address nor a relay")
        })?;
        let client = client::RpcClient::connect_wss_via_relay(
            &self.relay_inner_url,
            prev_id,
            prev_sig,
            &self.tls,
            relay,
        )
        .await?;
        Ok((client, ActiveLeg::WssRelay))
    }

    /// A single direct-only connect attempt (no relay fallback), used by the
    /// re-probe timer to migrate back to the direct path. Errors when the route
    /// has no direct address.
    pub(crate) async fn connect_direct(
        &self,
        prev_id: Option<&str>,
        prev_sig: Option<&str>,
    ) -> anyhow::Result<client::RpcClient> {
        let url = self
            .direct_url
            .as_ref()
            .ok_or_else(|| anyhow::Error::msg("no direct address to re-probe"))?;
        let fut = client::RpcClient::connect_wss_direct(url, prev_id, prev_sig, &self.tls);
        match tokio::time::timeout(Duration::from_secs(self.direct_timeout_secs.max(1)), fut).await
        {
            Ok(r) => r,
            Err(_) => Err(anyhow::Error::msg(format!(
                "direct re-probe to {url} timed out"
            ))),
        }
    }
}

/// Where zerocode should connect.
pub(crate) enum ConnectTarget {
    LocalSocket(PathBuf),
    // Boxed: `WssRoute` is much larger than the local-socket variant.
    Wss(Box<WssRoute>),
}

impl ConnectTarget {
    /// Human-readable label for the dashboard Status box.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::LocalSocket(p) => format!("local:{}", p.display()),
            Self::Wss(route) => match (&route.direct_url, &route.relay) {
                (Some(url), Some(r)) => format!("{url} (relay {} fallback)", r.relay_addr),
                (Some(url), None) => url.clone(),
                (None, Some(r)) => format!("relay {} -> {}", r.relay_addr, r.node_id),
                (None, None) => "wss".to_string(),
            },
        }
    }

    pub(crate) fn insecure_tls(&self) -> bool {
        matches!(self, Self::Wss(route) if route.tls.skip_verify)
    }

    /// Connect to this target, reclaiming a prior TUI identity when
    /// `prev_id`/`prev_sig` are supplied. Single source of truth for the
    /// per-transport connect call — used by initial startup and in-loop
    /// reconnection alike. Returns the leg the connection actually landed on.
    pub(crate) async fn connect(
        &self,
        prev_id: Option<&str>,
        prev_sig: Option<&str>,
    ) -> anyhow::Result<(client::RpcClient, ActiveLeg)> {
        match self {
            Self::LocalSocket(socket) => {
                let client = client::RpcClient::connect(socket, prev_id, prev_sig).await?;
                Ok((client, ActiveLeg::Local))
            }
            Self::Wss(route) => route.connect_preferred(prev_id, prev_sig).await,
        }
    }
}

/// In relay mode the inner WSS terminates at the daemon's own loopback listener
/// (the relay tunnels to it), so the inner server name is always the daemon's
/// self-SAN `127.0.0.1` and the port is cosmetic. When `--connect` is omitted on
/// a relay route we default to this so the common case is just `--relay`.
const DEFAULT_RELAY_INNER_URL: &str = "wss://127.0.0.1:9781";

/// Direct-first fallback defaults (overridable via `[connection.wss]`): try the
/// direct address this many times, with this per-attempt timeout, before falling
/// back to the relay; while on the relay, re-probe the direct path this often.
const DEFAULT_DIRECT_ATTEMPTS: u32 = 2;
const DEFAULT_DIRECT_TIMEOUT_SECS: u64 = 3;
const DEFAULT_REPROBE_SECS: u64 = 30;

/// The directly-reachable daemon address: CLI `--connect` overrides `[wss].uri`.
/// `None` means no direct address is configured (relay-only or local socket).
fn resolve_direct_url(cli_connect: Option<String>, cfg_wss: &config::WssSection) -> Option<String> {
    cli_connect.or_else(|| cfg_wss.uri.clone())
}

/// Server verification is skipped when either the flag or the config asks.
fn resolve_skip_verify(cli_skip_verify: bool, cfg_wss: &config::WssSection) -> bool {
    cli_skip_verify || cfg_wss.tls.skip_verify
}

fn should_enroll_via_relay(cli: &Cli, cfg_wss: &config::WssSection, relay_available: bool) -> bool {
    relay_available
        && cli.enroll_host.is_none()
        && cli.enroll_port.is_none()
        && (cli.relay.is_some() || (cli.connect.is_none() && cfg_wss.uri.is_none()))
}

async fn resolve_relay_dial(
    cli: &Cli,
    cfg_wss: &config::WssSection,
    cached_relay: Option<&enroll::RelayProfile>,
    config_dir: &std::path::Path,
) -> anyhow::Result<Option<client::RelayDial>> {
    // Relay coordinates: CLI -> config -> cached enrollment profile.
    let relay_addr = cli
        .relay
        .clone()
        .or_else(|| cfg_wss.relay_url.clone())
        .or_else(|| {
            cached_relay
                .map(|r| r.relay_url.clone())
                .filter(|s| !s.is_empty())
        });
    let relay_node = cli
        .relay_node
        .clone()
        .or_else(|| cfg_wss.relay_node.clone())
        .or_else(|| {
            cached_relay
                .map(|r| r.node_id.clone())
                .filter(|s| !s.is_empty())
        });

    // Relay outer-leaf pin candidates: --relay-pin -> the enrollment-delivered
    // pin -> a previously TOFU'd pin. The TLS connector ignores these candidates
    // when --relay-ca is set.
    let pin_store = config_dir.join("relay").join("relay_pin");
    let relay_pin = cli
        .relay_pin
        .clone()
        .or_else(|| {
            cached_relay
                .map(|r| r.relay_cert_pin.clone())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::fs::read_to_string(&pin_store)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    match (relay_addr, relay_node) {
        (Some(relay_addr), Some(node_id)) => {
            // Default the relay's expected cert name to its host:port host.
            let relay_host = cli.relay_host.clone().unwrap_or_else(|| {
                relay_addr
                    .rsplit_once(':')
                    .map(|(h, _)| h.to_string())
                    .unwrap_or_else(|| relay_addr.clone())
            });
            // Resolve the relay outer-cert trust: explicit trust settings skip
            // the prompt; otherwise offer interactive trust-on-first-use instead
            // of a bare UnknownIssuer at connect/enrollment time.
            let relay_pin =
                resolve_relay_trust(relay_pin, &relay_addr, &relay_host, cli, &pin_store).await?;
            Ok(Some(client::RelayDial {
                relay_addr,
                relay_host,
                node_id,
                relay_ca_path: cli.relay_ca.clone(),
                relay_insecure: cli.relay_insecure,
                relay_pin,
                relay_tofu: cli.relay_tofu,
                pin_store: Some(pin_store),
                outer_client_cert: cli.relay_client_cert.clone(),
                outer_client_key: cli.relay_client_key.clone(),
            }))
        }
        (None, None) => Ok(None),
        _ => Err(anyhow::Error::msg(
            "relay routing needs both a relay address and a node-id \
             (--relay + --relay-node, or wss.relay_url + wss.relay_node)",
        )),
    }
}

/// The credential facts every startup decision reads: the cached enrollment
/// profile (device id, renewal deadline, relay coordinates) and whether a client
/// certificate exists at all.
#[derive(Debug)]
struct CredentialStartup {
    profile: Option<enroll::CachedProfile>,
    certless: bool,
}

/// Recover an interrupted credential publication, VALIDATE the published
/// generation, THEN read the credential facts. The order is the contract: a
/// publication interrupted mid-rename has a new certificate beside an old
/// profile, so a read taken first reports stale relay coordinates or an absent
/// certificate that re-runs enrollment. A recovery that cannot reassemble the
/// generation is fatal here, because every route that follows would otherwise
/// dial with a mixed certificate/key set.
///
/// Validation runs second and covers the case recovery cannot see: a crash whose
/// marker did not survive, which leaves a mixed set with nothing pointing at it.
/// It must not run first, because during the marker window the recorded
/// generation is deliberately the previous one and recovery refreshes it.
fn recover_then_read_credentials(
    config_dir: &std::path::Path,
    cli_client_cert: Option<&str>,
    cfg_client_cert: &str,
) -> anyhow::Result<CredentialStartup> {
    enroll::recover_and_validate(config_dir)?;
    Ok(CredentialStartup {
        profile: enroll::cached_profile(config_dir),
        certless: enroll::is_certless(config_dir, cli_client_cert, cfg_client_cert),
    })
}

/// A default client-TLS file under `<config_dir>/tls/<name>`, if it exists, so a
/// client provisioned the conventional way needs no explicit `--tls-*` flags.
fn default_tls_path(config_dir: &std::path::Path, name: &str) -> Option<String> {
    let p = config_dir.join("tls").join(name);
    p.exists().then(|| p.to_string_lossy().into_owned())
}

/// Parse the host out of a `--connect` / `[wss].uri` value (`wss://host:port`,
/// `host:port`, or a bare `host`) for the enrollment endpoint. Naive for IPv6.
fn enroll_host_from(uri: Option<&str>) -> Option<String> {
    let uri = uri?.trim();
    let s = uri
        .strip_prefix("wss://")
        // Parsing a scheme prefix off a user-supplied URI, not opening a socket.
        .or_else(|| uri.strip_prefix("ws://")) // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
        .unwrap_or(uri);
    let s = s.split('/').next().unwrap_or(s);
    let host = s.rsplit_once(':').map(|(h, _)| h).unwrap_or(s);
    (!host.is_empty()).then(|| host.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    install_panic_hook();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zerocode: {}", format_startup_error(&e));
            ExitCode::FAILURE
        }
    }
}

fn format_startup_error(err: &anyhow::Error) -> String {
    if let Some(mismatch) = err.downcast_ref::<client::DaemonVersionMismatch>() {
        return i18n::t_args(
            "zc-error-daemon-version-mismatch",
            &[
                ("client_version", mismatch.client_version()),
                ("server_version", mismatch.server_version()),
            ],
        );
    }
    if let Some(timeout) = err.downcast_ref::<client::DaemonInitializeTimeout>() {
        return i18n::t_args(
            "zc-error-daemon-initialize-timeout",
            &[("seconds", &timeout.timeout_seconds().to_string())],
        );
    }
    if let Some(startup) = err.downcast_ref::<SpawnedDaemonStartupFailure>() {
        return i18n::t_args(
            "zc-error-spawned-daemon-startup",
            &[("details", startup.details())],
        );
    }
    format!("{err:#}")
}

/// Install a panic hook that restores the terminal before printing the
/// panic message.  Without this, a panic inside the event loop leaves the
/// terminal in raw mode / alternate screen, making the error unreadable.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        force_restore_terminal();
        default_hook(info);
    }));
}

/// Best-effort terminal restoration used by the panic hook and Unix shutdown
/// handlers. Errors are intentionally ignored — we're already crashing.
fn force_restore_terminal() {
    if TERMINAL_ACTIVE.load(Ordering::Relaxed) {
        // Terminal status outlives the process, so it has to be handed back
        // here too — otherwise a crash leaves the tab reading as busy.
        crate::osc_status::release();
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

enum InsecureTlsChoice {
    Once,
    Always,
    Abort,
}

/// Prompt the operator to accept an insecure-TLS connection to `url`.
///
/// Returns the operator's [`InsecureTlsChoice`]:
/// - [`InsecureTlsChoice::Once`] for `y` / `yes` (connect once, do not persist)
/// - [`InsecureTlsChoice::Always`] for `a` / `always` (connect and remember this route)
/// - [`InsecureTlsChoice::Abort`] for everything else (default, empty, `n`, junk)
///
/// Reads the operator's answer from `reader` and writes the prompt to
/// `writer` so tests can inject deterministic input without touching
/// `stdin` / `stderr`.
fn confirm_insecure_tls_with<R: std::io::BufRead, W: std::io::Write>(
    mut reader: R,
    writer: &mut W,
    url: &str,
) -> anyhow::Result<InsecureTlsChoice> {
    writeln!(
        writer,
        "\nWARNING: --tls-skip-verify DISABLES TLS certificate verification for\n\
         {url}\nThis connection is UNSAFE on untrusted networks (susceptible to\n\
         man-in-the-middle). Only continue on a trusted network against a\n\
         self-signed cert you control.\n\n\
         You are accepting an UNVERIFIED route, not a trusted peer.\n\
         [y] yes, connect once   [a] always (remember this route)   [N] no, abort"
    )?;
    write!(writer, "Continue with verification disabled? [y/a/N] ")?;
    writer.flush().ok();
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(InsecureTlsChoice::Once),
        "a" | "always" => Ok(InsecureTlsChoice::Always),
        _ => Ok(InsecureTlsChoice::Abort),
    }
}

/// Production entry point: locks `stdin` and writes the prompt to `stderr`,
/// delegating to [`confirm_insecure_tls_with`]. Behaviour is identical to
/// the previous inline implementation — the refactor only adds the
/// `BufRead` / `Write` seam so the prompt logic can be unit-tested.
fn confirm_insecure_tls(url: &str) -> anyhow::Result<InsecureTlsChoice> {
    let stdin = std::io::stdin();
    let mut stderr = std::io::stderr();
    confirm_insecure_tls_with(stdin.lock(), &mut stderr, url)
}

/// Operator choice when a relay presents an untrusted OUTER certificate.
#[derive(Debug, PartialEq, Eq)]
enum RelayTrustChoice {
    Trust,
    Abort,
}

/// Prompt seam (testable): show the relay's leaf fingerprint and ask whether to
/// trust + remember it. `reader`/`writer` are injected so the decision logic can
/// be unit-tested without a real terminal.
fn confirm_relay_cert_with<R: std::io::BufRead, W: std::io::Write>(
    mut reader: R,
    writer: &mut W,
    relay_addr: &str,
    fingerprint: &str,
) -> anyhow::Result<RelayTrustChoice> {
    writeln!(
        writer,
        "\nThe relay at {relay_addr} presented a certificate that is not yet trusted\n\
         (self-signed or an unknown issuer). Its SHA-256 fingerprint is:\n\n  \
         {fingerprint}\n\n\
         Confirm this matches the value the relay operator published, out of band.\n\
         Trusting it pins this leaf for future runs (under <config-dir>/relay). The\n\
         relay only forwards encrypted traffic; the inner mutual TLS to the daemon is\n\
         unaffected either way.\n\
         [y] trust and remember this relay   [N] abort"
    )?;
    write!(writer, "Trust this relay certificate? [y/N] ")?;
    writer.flush().ok();
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(RelayTrustChoice::Trust),
        _ => Ok(RelayTrustChoice::Abort),
    }
}

/// Production entry point: lock `stdin`, prompt on `stderr`.
fn confirm_relay_cert(relay_addr: &str, fingerprint: &str) -> anyhow::Result<RelayTrustChoice> {
    let stdin = std::io::stdin();
    let mut stderr = std::io::stderr();
    confirm_relay_cert_with(stdin.lock(), &mut stderr, relay_addr, fingerprint)
}

/// Resolve the relay's OUTER-certificate trust. When trust was set explicitly
/// (`--relay-ca`/`--relay-pin`/`--relay-tofu`/`--relay-insecure`, or a remembered
/// pin) it is used as-is. Otherwise, rather than failing the connect with a bare
/// `UnknownIssuer`, fetch the relay leaf, show its fingerprint, and offer to trust
/// and remember it (interactive only); a non-interactive run gets an actionable
/// error pointing at the explicit flags.
async fn resolve_relay_trust(
    existing_pin: Option<String>,
    relay_addr: &str,
    relay_host: &str,
    cli: &Cli,
    pin_store: &std::path::Path,
) -> anyhow::Result<Option<String>> {
    use std::io::IsTerminal as _;
    let explicit =
        cli.relay_ca.is_some() || cli.relay_insecure || cli.relay_tofu || existing_pin.is_some();
    if explicit {
        return Ok(existing_pin);
    }
    if !std::io::stderr().is_terminal() {
        return Err(anyhow::Error::msg(format!(
            "the relay at {relay_addr} presents an untrusted certificate and no relay \
             trust was configured. Pass --relay-ca <file>, --relay-pin <sha256>, or \
             --relay-tofu (this run is non-interactive, so it cannot prompt)."
        )));
    }
    let fp = match client::probe_relay_cert_pin(relay_addr, relay_host).await {
        Ok(fp) => fp,
        // Probe failed (relay unreachable etc.): let the normal connect surface it.
        Err(_) => return Ok(existing_pin),
    };
    match confirm_relay_cert(relay_addr, &fp)? {
        RelayTrustChoice::Trust => {
            client::persist_relay_pin(pin_store, &fp);
            eprintln!("Trusted the relay; pinned {fp} for future connections.");
            Ok(Some(fp))
        }
        RelayTrustChoice::Abort => Err(anyhow::Error::msg(
            "relay certificate not trusted; aborting.",
        )),
    }
}

#[cfg(unix)]
fn prepare_wss_termination_signals(
    requires_confirmation: bool,
    config_dir: &std::path::Path,
    url: &str,
) -> anyhow::Result<TerminationSignals> {
    // Keep the default signal disposition while this synchronous prompt waits
    // for input. Installing Tokio's handler first would consume Ctrl+C without
    // giving the blocked read a chance to observe it.
    if requires_confirmation {
        apply_insecure_tls_choice(confirm_insecure_tls(url)?, config_dir, url)?;
    }
    Ok(TerminationSignals::new()?)
}

/// Apply the operator's [`InsecureTlsChoice`] for `url`: a no-op for
/// `Once`, persisting the route acknowledgement for `Always`, or
/// bailing out for `Abort`. Extracted from the inline match in `run()`
/// so the choice -> side-effect mapping can be exercised directly in
/// tests without a running daemon.
fn apply_insecure_tls_choice(
    choice: InsecureTlsChoice,
    config_dir: &std::path::Path,
    url: &str,
) -> anyhow::Result<()> {
    match choice {
        InsecureTlsChoice::Once => {}
        InsecureTlsChoice::Always => {
            config::persist_wss_route_ack(config_dir, url)?;
        }
        InsecureTlsChoice::Abort => {
            anyhow::bail!("aborted: insecure TLS connection not confirmed");
        }
    }
    Ok(())
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let _ = rustls::crypto::ring::default_provider().install_default();

    let local_config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
    let loaded_config = match config::ensure_and_load(&local_config_dir) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("zerocode: config load failed ({e:#}); starting with defaults");
            config::ZerocodeConfig::default()
        }
    };
    let active_theme = loaded_config.resolve_theme().unwrap_or_else(|e| {
        let path = config::config_path(&local_config_dir);
        eprintln!("zerocode: {e:#}");
        eprintln!(
            "  fix: remove the entire [theme] section from {} to restore the default theme",
            path.display()
        );
        std::process::exit(1);
    });
    theme::set_active(active_theme);

    let resolved_locale = loaded_config
        .resolve_locale()
        .unwrap_or_else(i18n::detect_locale);
    i18n::init(&resolved_locale, &local_config_dir);

    // Apply persisted keybinding overrides into the keymap. A bad table
    // fails loud (same posture as an unknown theme) rather than silently
    // running stale bindings.
    match loaded_config.resolve_keybindings() {
        Ok(table) if !table.is_empty() => keymap::overrides::set_active(table),
        Ok(_) => {}
        Err(e) => {
            let path = config::config_path(&local_config_dir);
            eprintln!("zerocode: invalid keybindings: {e:#}");
            eprintln!(
                "  fix: remove the entire [keybindings] section from {} to restore default keybindings",
                path.display()
            );
            std::process::exit(1);
        }
    }

    // Enrollment: if a remote (WSS) connection is intended but no client cert is
    // available, obtain one first (explicitly via --enroll, or automatically on an
    // interactive --connect). The cert is cached under <config-dir>/tls, so the
    // target block below picks it up with no --tls-* flags.
    {
        use std::io::IsTerminal as _;
        let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
        let cfg_wss = &loaded_config.connection.wss;
        // Recovery runs before the first credential read of the process, so no
        // decision below observes a half-published generation.
        let credentials = recover_then_read_credentials(
            &config_dir,
            cli.tls_client_cert.as_deref(),
            &cfg_wss.tls.client_cert_path,
        )?;
        let cached_relay = credentials.profile.as_ref().map(|p| &p.relay);
        let relay = resolve_relay_dial(&cli, cfg_wss, cached_relay, &config_dir).await?;
        let wss_intended = cli.connect.is_some()
            || cfg_wss.uri.is_some()
            || cli.relay.is_some()
            || cfg_wss.relay_url.is_some();
        let auto = wss_intended
            && credentials.certless
            && cli.connect.is_some()
            && std::io::stderr().is_terminal();
        if cli.enroll || auto {
            if should_enroll_via_relay(&cli, cfg_wss, relay.is_some()) {
                enroll::enroll_via_relay(
                    relay.as_ref().expect("checked relay.is_some()"),
                    &config_dir,
                )
                .await?;
            } else {
                let host = cli
                    .enroll_host
                    .clone()
                    .or_else(|| enroll_host_from(cli.connect.as_deref()))
                    .or_else(|| enroll_host_from(cfg_wss.uri.as_deref()))
                    .ok_or_else(|| {
                        anyhow::Error::msg(
                            "enrollment needs a host: pass --enroll-host, --connect wss://<host>:<port>, or --relay with --relay-node",
                        )
                    })?;
                let port = cli.enroll_port.unwrap_or(enroll::DEFAULT_ENROLL_PORT);
                enroll::enroll(&host, port, &config_dir).await?;
            }
        }
    }

    let target = {
        let cfg_wss = &loaded_config.connection.wss;
        let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
        // The cached enrollment profile supplies the relay coordinates so a bare
        // `zerocode` after enrollment still reaches the daemon through its relay.
        // Enrollment above may have published a new generation, so recover and
        // re-read here too rather than reusing the pre-enrollment facts. A
        // recovery that cannot reassemble the generation aborts the run instead
        // of resolving relay coordinates or TLS paths from a mixed set.
        let credentials = recover_then_read_credentials(
            &config_dir,
            cli.tls_client_cert.as_deref(),
            &cfg_wss.tls.client_cert_path,
        )?;
        let cached_relay = credentials.profile.as_ref().map(|p| &p.relay);

        // Direct daemon address (CLI overrides config). `None` => relay-only.
        let direct_url = resolve_direct_url(cli.connect.clone(), cfg_wss);
        let skip_verify = resolve_skip_verify(cli.tls_skip_verify, cfg_wss);

        let relay = resolve_relay_dial(&cli, cfg_wss, cached_relay, &config_dir).await?;

        // A WSS route is chosen when a direct address OR a relay is available;
        // otherwise the local IPC socket.
        if direct_url.is_some() || relay.is_some() {
            // Mutual-TLS material: CLI flag -> config -> conventional default path
            // under <config_dir>/tls, so a provisioned client needs no --tls-* flags.
            let tls = client::ClientTls {
                skip_verify,
                ca_cert_path: cli
                    .tls_ca_cert
                    .clone()
                    .or_else(|| opt_path(&cfg_wss.tls.ca_cert_path))
                    .or_else(|| default_tls_path(&config_dir, "ca.crt")),
                client_cert_path: cli
                    .tls_client_cert
                    .clone()
                    .or_else(|| opt_path(&cfg_wss.tls.client_cert_path))
                    .or_else(|| default_tls_path(&config_dir, "client.crt")),
                client_key_path: cli
                    .tls_client_key
                    .clone()
                    .or_else(|| opt_path(&cfg_wss.tls.client_key_path))
                    .or_else(|| default_tls_path(&config_dir, "client.key")),
            };
            ConnectTarget::Wss(Box::new(WssRoute {
                direct_url,
                relay_inner_url: DEFAULT_RELAY_INNER_URL.to_string(),
                relay,
                tls,
                direct_attempts: cfg_wss.direct_attempts.unwrap_or(DEFAULT_DIRECT_ATTEMPTS),
                direct_timeout_secs: cfg_wss
                    .direct_timeout_secs
                    .unwrap_or(DEFAULT_DIRECT_TIMEOUT_SECS),
                reprobe_secs: cfg_wss.reprobe_secs.unwrap_or(DEFAULT_REPROBE_SECS),
            }))
        } else {
            let socket = client::resolve_socket_path(&config_dir)?;
            ConnectTarget::LocalSocket(socket)
        }
    };

    #[cfg(unix)]
    let mut termination_signals = None;

    // Initial connection (before the terminal is initialized).
    // `owns_ephemeral` records whether THIS process spawned the daemon
    // (initial connect failed → we started one). Only an owned ephemeral
    // daemon may be respawned on disconnect, and then exactly once.
    let mut owns_ephemeral = false;
    let (rpc, initial_leg) = match &target {
        ConnectTarget::LocalSocket(socket) => {
            #[cfg(unix)]
            let termination_signals = termination_signals.insert(TerminationSignals::new()?);
            #[cfg(unix)]
            let initial_connection = tokio::select! {
                result = client::RpcClient::connect(socket, None, None) => result,
                _ = termination_signals.recv() => return Ok(()),
            };
            #[cfg(not(unix))]
            let initial_connection = client::RpcClient::connect(socket, None, None).await;

            let client = match initial_connection {
                Ok(c) => c,
                Err(e) if is_terminal_connection_error(&e) => return Err(e),
                Err(_) => {
                    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
                    let mut daemon = spawn_owned_ephemeral_daemon(&config_dir, socket)?;
                    #[cfg(unix)]
                    let readiness = tokio::select! {
                        result = await_spawned_daemon_ready(socket, &mut daemon) => result,
                        _ = termination_signals.recv() => {
                            return cleanup_spawned_daemon_after_signal(&mut daemon);
                        }
                    };
                    #[cfg(not(unix))]
                    let readiness = await_spawned_daemon_ready(socket, &mut daemon).await;

                    match readiness {
                        Ok(client) => {
                            owns_ephemeral =
                                reconcile_spawned_daemon_identity(client.server_pid, &mut daemon)?;
                            if owns_ephemeral {
                                daemon.detach();
                            }
                            client
                        }
                        Err(startup_error) => {
                            return Err(spawned_daemon_startup_failure(startup_error, &mut daemon));
                        }
                    }
                }
            };
            (client, ActiveLeg::Local)
        }
        ConnectTarget::Wss(route) => {
            let ack_key = route.ack_key();
            let requires_confirmation =
                route.tls.skip_verify && !loaded_config.connection.wss.tls.route_acked(&ack_key);
            #[cfg(not(unix))]
            if requires_confirmation {
                apply_insecure_tls_choice(
                    confirm_insecure_tls(&ack_key)?,
                    &local_config_dir,
                    &ack_key,
                )?;
            }
            #[cfg(unix)]
            let termination_signals = termination_signals.insert(prepare_wss_termination_signals(
                requires_confirmation,
                &local_config_dir,
                &ack_key,
            )?);
            // Signal-aware like the local leg: `connect_preferred` can spend the
            // full direct-attempt budget before falling back to the relay, so a
            // SIGTERM during that window must still exit cleanly.
            #[cfg(unix)]
            let connect_result = tokio::select! {
                result = route.connect_preferred(None, None) => result,
                _ = termination_signals.recv() => return Ok(()),
            };
            #[cfg(not(unix))]
            let connect_result = route.connect_preferred(None, None).await;

            match connect_result {
                Ok(pair) => pair,
                // A certless client cannot complete the mutually-authenticated WSS
                // handshake. Give an actionable enroll hint instead of a bare TLS
                // error (no silent failure for an un-migrated client).
                Err(e) if route.tls.client_cert_path.is_none() => {
                    anyhow::bail!(
                        "could not connect to the daemon's WSS plane ({e:#}). That plane is \
                         mutually authenticated and this client has no certificate. Enroll first:\n  \
                         zerocode --enroll --connect <host>:<port>\n(running interactively against \
                         --connect enrolls automatically)."
                    );
                }
                Err(e) => return Err(e),
            }
        }
    };

    // On the mTLS plane, renew the cached client cert if it is past ~50% of its
    // TTL (before the terminal is taken over, so any output is visible). No-op
    // when the client never enrolled here.
    if matches!(target, ConnectTarget::Wss(_)) {
        enroll::maybe_renew(&rpc, &local_config_dir).await;
    }

    let mut term = config_manager::init_terminal()?;
    TERMINAL_ACTIVE.store(true, Ordering::Relaxed);

    let result = run_until_exit(
        Arc::new(rpc),
        &mut term,
        &target,
        &local_config_dir,
        owns_ephemeral,
        initial_leg,
        #[cfg(unix)]
        termination_signals
            .as_mut()
            .expect("Unix connection path installs termination signal handlers"),
    )
    .await;

    TERMINAL_ACTIVE.store(false, Ordering::Relaxed);
    config_manager::restore_terminal(&mut term)?;
    result
}

/// Runs the TUI under Unix termination handlers so the terminal is restored
/// instead of dying mid-draw. `app::run` owns the full session
/// lifecycle — including in-loop reconnection and recovery — and returns
/// only when the user quits.
async fn run_until_exit(
    rpc: Arc<client::RpcClient>,
    term: &mut config_manager::Term,
    target: &ConnectTarget,
    config_dir: &std::path::Path,
    owns_ephemeral: bool,
    initial_leg: ActiveLeg,
    #[cfg(unix)] termination_signals: &mut TerminationSignals,
) -> anyhow::Result<()> {
    // Shared state that survives a reconnect. Quickstart's Stage 2 writes
    // the new agent's alias here so the recovering `app::run` loop drops
    // the user into Chat once the daemon is back up.
    let reconnect_state: app::SharedReconnectState =
        Arc::new(std::sync::Mutex::new(app::CrossReconnectState::default()));

    let label = target.label();
    let insecure_tls = target.insecure_tls();

    #[cfg(unix)]
    {
        tokio::select! {
            r = app::run(rpc, term, &label, insecure_tls, reconnect_state, config_dir, target, owns_ephemeral, initial_leg) => r.map(|_| ()),
            _ = termination_signals.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        app::run(
            rpc,
            term,
            &label,
            insecure_tls,
            reconnect_state,
            config_dir,
            target,
            owns_ephemeral,
            initial_leg,
        )
        .await
        .map(|_| ())
    }
}

pub(crate) fn spawn_ephemeral_daemon(
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) -> anyhow::Result<()> {
    let mut cmd = ephemeral_daemon_command(config_dir, socket);
    cmd.stderr(std::process::Stdio::null());
    cmd.spawn()
        .map_err(|e| anyhow::Error::msg(format!("failed to spawn daemon: {e}")))?;
    Ok(())
}

fn spawn_owned_ephemeral_daemon(
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) -> anyhow::Result<SpawnedDaemon> {
    SpawnedDaemon::spawn(ephemeral_daemon_command(config_dir, socket))
}

fn ephemeral_daemon_command(
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) -> std::process::Command {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("zeroclaw")))
        .unwrap_or_else(|| PathBuf::from("zeroclaw"));

    let mut cmd = std::process::Command::new(&exe);
    configure_ephemeral_daemon_command(&mut cmd, config_dir, socket);

    // Lower the daemon's log level to DEBUG when spawned ephemerally by
    // zerocode so that the Logs pane can show debug events without any
    // manual RUST_LOG override. Third-party crates stay at WARN to avoid
    // noise. Honour an existing RUST_LOG if the user set one themselves.
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env(
            "RUST_LOG",
            "debug,matrix_sdk=warn,matrix_sdk_base=warn,matrix_sdk_crypto=warn,\
             hyper=warn,reqwest=warn,tokio=warn,h2=warn",
        );
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    cmd
}

fn configure_ephemeral_daemon_command(
    cmd: &mut std::process::Command,
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) {
    cmd.arg("daemon")
        .arg("--ephemeral")
        .arg("--config-dir")
        .arg(config_dir)
        // The TUI waits on this exact endpoint, so the child must bind it
        // instead of independently deriving a potentially different path.
        .env("ZEROCLAW_SOCKET", socket);
}

struct SpawnedDaemon {
    child: std::process::Child,
    stderr: Arc<Mutex<std::collections::VecDeque<u8>>>,
    capture_stderr: Arc<AtomicBool>,
    stderr_done: Option<std::sync::mpsc::Receiver<()>>,
    stderr_collector: Option<std::thread::JoinHandle<()>>,
    cleanup_on_drop: bool,
}

impl SpawnedDaemon {
    fn spawn(mut cmd: std::process::Command) -> anyhow::Result<Self> {
        use std::io::Read;

        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::Error::msg(format!("failed to spawn daemon: {e}")))?;
        let mut stderr_pipe = match child.stderr.take() {
            Some(pipe) => pipe,
            None => {
                let cleanup_error = terminate_child(&mut child).err();
                let mut message = "spawned daemon stderr pipe was unavailable".to_owned();
                if let Some(error) = cleanup_error {
                    message.push_str("; cleanup also failed: ");
                    message.push_str(&format!("{error:#}"));
                }
                return Err(anyhow::Error::msg(message));
            }
        };
        let stderr = Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(
            DAEMON_STDERR_LIMIT,
        )));
        let capture_stderr = Arc::new(AtomicBool::new(true));
        let collector_buffer = Arc::clone(&stderr);
        let collector_capture = Arc::clone(&capture_stderr);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let stderr_collector = match std::thread::Builder::new()
            .name("zerocode-daemon-stderr".to_owned())
            .spawn(move || {
                let mut chunk = [0_u8; 1024];
                while let Ok(read) = stderr_pipe.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    if !collector_capture.load(Ordering::Acquire) {
                        continue;
                    }
                    let mut buffer = collector_buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !collector_capture.load(Ordering::Acquire) {
                        continue;
                    }
                    buffer.extend(&chunk[..read]);
                    while buffer.len() > DAEMON_STDERR_LIMIT {
                        buffer.pop_front();
                    }
                }
                let _ = done_tx.send(());
            }) {
            Ok(collector) => collector,
            Err(spawn_error) => {
                let cleanup_error = terminate_child(&mut child).err();
                let mut message = format!("failed to start daemon stderr collector: {spawn_error}");
                if let Some(error) = cleanup_error {
                    message.push_str("; cleanup also failed: ");
                    message.push_str(&format!("{error:#}"));
                }
                return Err(anyhow::Error::msg(message));
            }
        };

        Ok(Self {
            child,
            stderr,
            capture_stderr,
            stderr_done: Some(done_rx),
            stderr_collector: Some(stderr_collector),
            cleanup_on_drop: true,
        })
    }

    #[cfg(test)]
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn poll_exit(&mut self) -> anyhow::Result<Option<SpawnedDaemonExit>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        let stderr = self.finish_stderr_collection();
        Ok(Some(SpawnedDaemonExit { status, stderr }))
    }

    fn terminate_and_wait(&mut self) -> anyhow::Result<SpawnedDaemonExit> {
        let status = terminate_child(&mut self.child);
        let stderr = self.finish_stderr_collection();
        status.map(|status| SpawnedDaemonExit { status, stderr })
    }

    fn finish_stderr_collection(&mut self) -> String {
        if let Some(done) = self.stderr_done.take() {
            let _ = done.recv_timeout(Duration::from_millis(100));
        }
        self.capture_stderr.store(false, Ordering::Release);

        let mut buffer = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = buffer.drain(..).collect::<Vec<_>>();
        drop(buffer);

        self.stderr_collector.take();
        sanitize_daemon_stderr(&bytes)
    }

    fn detach(mut self) {
        self.cleanup_on_drop = false;
        self.capture_stderr.store(false, Ordering::Release);
        self.stderr_done.take();
        self.stderr_collector.take();
    }
}

impl Drop for SpawnedDaemon {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = self.terminate_and_wait();
        }
    }
}

fn terminate_child(child: &mut std::process::Child) -> anyhow::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }

    if let Err(kill_error) = child.kill() {
        return match child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) => Err(anyhow::Error::msg(format!(
                "failed to terminate daemon: {kill_error}"
            ))),
            Err(poll_error) => Err(anyhow::Error::msg(format!(
                "failed to terminate daemon: {kill_error}; failed to re-check daemon: {poll_error}"
            ))),
        };
    }

    child
        .wait()
        .map_err(|error| anyhow::Error::msg(format!("failed to reap daemon: {error}")))
}

fn sanitize_daemon_stderr(bytes: &[u8]) -> String {
    let mut rendered = String::new();
    let decoded = String::from_utf8_lossy(bytes);
    let mut characters = decoded.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' if characters.peek() == Some(&'\n') => {
                characters.next();
                rendered.push('\n');
            }
            '\r' => rendered.push('\u{fffd}'),
            '\n' | '\t' => rendered.push(character),
            character if character.is_control() => rendered.push('\u{fffd}'),
            character => rendered.push(character),
        }
    }

    if rendered.len() <= DAEMON_STDERR_LIMIT {
        return rendered;
    }
    let mut start = rendered.len() - DAEMON_STDERR_LIMIT;
    while !rendered.is_char_boundary(start) {
        start += 1;
    }
    rendered[start..].to_owned()
}

fn reconcile_spawned_daemon_identity(
    server_pid: Option<u32>,
    daemon: &mut SpawnedDaemon,
) -> anyhow::Result<bool> {
    if server_pid == Some(daemon.id()) {
        return Ok(true);
    }

    let spawned_pid = daemon.id();
    daemon.terminate_and_wait().map_err(|cleanup_error| {
        anyhow::Error::msg(format!(
            "connected to daemon pid {}, but failed to clean up spawned daemon pid {}: {cleanup_error:#}",
            server_pid.map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()),
            spawned_pid,
        ))
    })?;

    if server_pid.is_none() {
        anyhow::bail!(
            "spawned daemon did not report its process id during initialization; cleaned up pid {spawned_pid}"
        );
    }
    Ok(false)
}

#[cfg(unix)]
fn cleanup_spawned_daemon_after_signal(daemon: &mut SpawnedDaemon) -> anyhow::Result<()> {
    daemon
        .terminate_and_wait()
        .map(|_| ())
        .map_err(|cleanup_error| {
            anyhow::Error::msg(format!(
                "received shutdown signal while starting daemon; cleanup failed: {cleanup_error:#}"
            ))
        })
}

#[derive(Debug)]
struct SpawnedDaemonExit {
    status: ExitStatus,
    stderr: String,
}

impl SpawnedDaemonExit {
    #[cfg(test)]
    fn stderr(&self) -> &str {
        &self.stderr
    }
}

impl std::fmt::Display for SpawnedDaemonExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "daemon exited before ready (status: {})",
            self.status
        )?;
        if !self.stderr.trim().is_empty() {
            write!(formatter, "; stderr: {}", self.stderr.trim())?;
        }
        Ok(())
    }
}

impl std::error::Error for SpawnedDaemonExit {}

#[derive(Debug)]
struct SpawnedDaemonStartupFailure {
    details: String,
}

impl SpawnedDaemonStartupFailure {
    fn details(&self) -> &str {
        &self.details
    }
}

impl std::fmt::Display for SpawnedDaemonStartupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.details)
    }
}

impl std::error::Error for SpawnedDaemonStartupFailure {}

fn spawned_daemon_startup_failure(
    startup_error: anyhow::Error,
    daemon: &mut SpawnedDaemon,
) -> anyhow::Error {
    let startup_exit = startup_error.downcast_ref::<SpawnedDaemonExit>();
    let mut details = if let Some(exit) = startup_exit {
        format!("daemon exited before ready (status: {})", exit.status)
    } else {
        format_startup_error(&startup_error)
    };

    let cleanup = daemon.terminate_and_wait();
    let stderr = startup_exit
        .map(|exit| exit.stderr.as_str())
        .filter(|stderr| !stderr.trim().is_empty())
        .or_else(|| {
            cleanup
                .as_ref()
                .ok()
                .map(|exit| exit.stderr.as_str())
                .filter(|stderr| !stderr.trim().is_empty())
        })
        .unwrap_or_default();
    if !stderr.trim().is_empty() {
        details.push_str("; stderr: ");
        details.push_str(stderr.trim());
    }
    if let Err(error) = cleanup {
        details.push_str("; cleanup also failed: ");
        details.push_str(&format!("{error:#}"));
    }

    anyhow::Error::new(SpawnedDaemonStartupFailure { details })
}

async fn await_spawned_daemon_ready(
    socket: &std::path::Path,
    daemon: &mut SpawnedDaemon,
) -> anyhow::Result<client::RpcClient> {
    let deadline = tokio::time::Instant::now() + SPAWNED_DAEMON_CONNECT_TIMEOUT;
    loop {
        if let Some(exit) = daemon.poll_exit()? {
            return Err(anyhow::Error::new(exit));
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon did not become ready within {}s (socket: {})",
                SPAWNED_DAEMON_CONNECT_TIMEOUT.as_secs(),
                socket.display(),
            );
        }
        match client::RpcClient::connect(socket, None, None).await {
            Ok(c) => return Ok(c),
            Err(e) if is_terminal_connection_error(&e) => return Err(e),
            Err(_) => tokio::time::sleep(DAEMON_CONNECT_INTERVAL).await,
        }
    }
}

fn is_daemon_version_mismatch(err: &anyhow::Error) -> bool {
    err.downcast_ref::<client::DaemonVersionMismatch>()
        .is_some()
}

fn is_terminal_connection_error(err: &anyhow::Error) -> bool {
    is_daemon_version_mismatch(err)
        || err
            .downcast_ref::<client::DaemonInitializeTimeout>()
            .is_some()
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::config::WssSection;
    use std::ffi::OsStr;

    fn spawned_daemon_helper_command(mode: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new(
            std::env::current_exe().expect("current zerocode test binary path"),
        );
        cmd.args([
            "connection_tests::spawned_daemon_subprocess_helper",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("ZEROCODE_SPAWNED_DAEMON_HELPER", mode);
        cmd
    }

    #[test]
    fn spawned_daemon_cleanup_terminates_and_reaps_running_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");
        assert!(daemon.try_wait().expect("poll helper").is_none());

        let exit = daemon.terminate_and_wait().expect("terminate helper");

        assert!(!exit.status.success());
        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[test]
    fn spawned_daemon_early_exit_reports_bounded_stderr() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("stderr")).expect("spawn helper");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let exit = loop {
            if let Some(exit) = daemon.poll_exit().expect("poll helper") {
                break exit;
            }
            assert!(std::time::Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };
        let rendered = exit.to_string();

        assert!(rendered.contains("status"));
        assert!(rendered.contains("spawned-daemon-stderr-tail"));
        assert!(exit.stderr().len() <= DAEMON_STDERR_LIMIT);
    }

    #[test]
    fn spawned_daemon_exit_does_not_wait_for_inherited_stderr() {
        let mut exercise = spawned_daemon_helper_command("exercise-inherited-stderr")
            .spawn()
            .expect("spawn inherited-stderr exercise");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);

        loop {
            if let Some(status) = exercise.try_wait().expect("poll exercise") {
                assert!(status.success(), "exercise failed with {status}");
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = exercise.kill();
                let _ = exercise.wait();
                panic!("poll_exit blocked while a descendant held stderr open");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn spawned_daemon_stderr_is_rendered_safely_within_limit() {
        let mut daemon = SpawnedDaemon::spawn(spawned_daemon_helper_command("unsafe-stderr"))
            .expect("spawn helper");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let exit = loop {
            if let Some(exit) = daemon.poll_exit().expect("poll helper") {
                break exit;
            }
            assert!(std::time::Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };

        assert!(exit.stderr().len() <= DAEMON_STDERR_LIMIT);
        assert!(!exit.stderr().contains('\u{1b}'));
        assert!(!exit.stderr().contains('\0'));
        assert!(!exit.stderr().contains('\r'));
        assert!(exit.stderr().contains("unsafe-stderr-tail"));
    }

    #[test]
    fn spawned_daemon_stderr_normalizes_crlf_and_replaces_bare_carriage_returns() {
        assert_eq!(
            sanitize_daemon_stderr(b"first\r\nsecond\roverwrite"),
            "first\nsecond\u{fffd}overwrite"
        );
    }

    #[test]
    fn spawned_daemon_mismatched_identity_terminates_and_reaps_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");
        let spawned_pid = daemon.id();

        let owns_ephemeral =
            reconcile_spawned_daemon_identity(Some(spawned_pid.wrapping_add(1)), &mut daemon)
                .expect("clean up mismatched child");

        assert!(!owns_ephemeral);
        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[test]
    fn spawned_daemon_missing_identity_fails_closed_after_reaping_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");

        let error = reconcile_spawned_daemon_identity(None, &mut daemon)
            .expect_err("missing identity must not transfer ownership");

        assert!(error.to_string().contains("did not report its process id"));
        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[test]
    fn spawned_daemon_matching_identity_retains_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");

        let owns_ephemeral = reconcile_spawned_daemon_identity(Some(daemon.id()), &mut daemon)
            .expect("accept matching child");

        assert!(owns_ephemeral);
        assert!(daemon.try_wait().expect("poll helper").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn spawned_daemon_startup_signal_cleanup_terminates_and_reaps_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");

        cleanup_spawned_daemon_after_signal(&mut daemon).expect("clean up signalled child");

        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[cfg(unix)]
    fn assert_parent_signal_cleans_up_child(signal: libc::c_int) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let pid_path = temp.path().join("daemon.pid");
        let mut owner = spawned_daemon_helper_command("signal-owner");
        owner.env("ZEROCODE_SIGNAL_OWNER_PID_PATH", &pid_path);
        let mut owner = owner.spawn().expect("spawn signal owner");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let daemon_pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_path) {
                let pid = pid.trim();
                if !pid.is_empty() {
                    break pid.parse::<u32>().expect("parse daemon pid");
                }
            }
            assert!(
                owner.try_wait().expect("poll signal owner").is_none(),
                "signal owner exited before publishing daemon pid"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "signal owner did not publish daemon pid"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        // SAFETY: `owner.id()` is the live child PID returned by `spawn`; this
        // test sends a standard signal and does not pass pointers across FFI.
        let signal_result = unsafe { libc::kill(owner.id() as libc::pid_t, signal) };
        assert_eq!(
            signal_result,
            0,
            "send parent signal: {}",
            std::io::Error::last_os_error()
        );
        assert!(owner.wait().expect("wait signal owner").success());

        // SAFETY: signal 0 performs a process-existence probe only; `daemon_pid`
        // was parsed from the child helper's PID file and no pointers are used.
        let child_probe = unsafe { libc::kill(daemon_pid as libc::pid_t, 0) };
        assert_eq!(child_probe, -1, "spawned daemon pid {daemon_pid} survived");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawned_daemon_parent_termination_signals_clean_up_child() {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
            assert_parent_signal_cleans_up_child(signal);
        }
    }

    #[cfg(unix)]
    #[test]
    fn insecure_tls_prompt_preserves_default_sigint() {
        use std::io::Read;
        use std::os::unix::process::ExitStatusExt;

        let temp = tempfile::tempdir().expect("create prompt temp dir");
        let mut owner = spawned_daemon_helper_command("insecure-tls-prompt-owner");
        owner
            .env("ZEROCODE_INSECURE_PROMPT_CONFIG_DIR", temp.path())
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut owner = owner.spawn().expect("spawn insecure-TLS prompt owner");
        let mut stderr = owner.stderr.take().expect("capture prompt stderr");
        let (prompt_tx, prompt_rx) = std::sync::mpsc::channel();
        let prompt_reader = std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut byte = [0_u8; 1];
            while stderr.read(&mut byte).expect("read prompt stderr") != 0 {
                output.push(byte[0]);
                if output.ends_with(b"Continue with verification disabled? [y/a/N] ") {
                    prompt_tx.send(()).expect("publish prompt readiness");
                    return;
                }
            }
            panic!("prompt owner exited before writing the confirmation prompt");
        });
        if prompt_rx.recv_timeout(Duration::from_secs(5)).is_err() {
            let _ = owner.kill();
            let _ = owner.wait();
            prompt_reader.join().expect("join prompt reader");
            panic!("prompt owner did not block for insecure-TLS confirmation");
        }

        let signal_result = unsafe { libc::kill(owner.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(
            signal_result,
            0,
            "send SIGINT to prompt owner: {}",
            std::io::Error::last_os_error()
        );

        let exit_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = owner.try_wait().expect("poll signalled prompt owner") {
                break status;
            }
            if std::time::Instant::now() >= exit_deadline {
                let _ = owner.kill();
                let _ = owner.wait();
                panic!("SIGINT was swallowed while the insecure-TLS prompt was blocked");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.signal(), Some(libc::SIGINT));
        prompt_reader.join().expect("join prompt reader");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn duplicate_stdio(fd: libc::c_int) -> std::process::Stdio {
        use std::os::fd::FromRawFd;

        let duplicate = unsafe { libc::dup(fd) };
        assert!(
            duplicate >= 0,
            "duplicate PTY slave: {}",
            std::io::Error::last_os_error()
        );
        unsafe { std::process::Stdio::from_raw_fd(duplicate) }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn terminal_attributes(fd: libc::c_int) -> libc::termios {
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        let result = unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) };
        assert_eq!(
            result,
            0,
            "read PTY terminal attributes: {}",
            std::io::Error::last_os_error()
        );
        unsafe { attributes.assume_init() }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn assert_terminal_attributes_equal(expected: &libc::termios, actual: &libc::termios) {
        assert_eq!(actual.c_iflag, expected.c_iflag, "input flags not restored");
        assert_eq!(
            actual.c_oflag, expected.c_oflag,
            "output flags not restored"
        );
        assert_eq!(
            actual.c_cflag, expected.c_cflag,
            "control flags not restored"
        );
        assert_eq!(actual.c_lflag, expected.c_lflag, "local flags not restored");
        assert_eq!(
            actual.c_cc, expected.c_cc,
            "control characters not restored"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn output_contains(output: &[u8], sequence: &[u8]) -> bool {
        output
            .windows(sequence.len())
            .any(|candidate| candidate == sequence)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn set_nonblocking(fd: libc::c_int) {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(
            flags >= 0,
            "read PTY flags: {}",
            std::io::Error::last_os_error()
        );
        let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(
            result,
            0,
            "set PTY nonblocking: {}",
            std::io::Error::last_os_error()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn drain_pty_output(master: &mut std::fs::File, output: &mut Vec<u8>) {
        use std::io::Read;

        let mut buffer = [0_u8; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => output.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // Linux PTY masters report EIO, rather than EOF, after the
                // final slave descriptor closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("read terminal output: {error}"),
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn assert_signal_restores_terminal(signal: libc::c_int) {
        use std::os::fd::FromRawFd;

        let mut master_fd = -1;
        let mut slave_fd = -1;
        let openpty_result = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            openpty_result,
            0,
            "open PTY: {}",
            std::io::Error::last_os_error()
        );
        let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
        set_nonblocking(master_fd);
        let original = terminal_attributes(slave_fd);

        let temp = tempfile::tempdir().expect("create terminal-owner temp dir");
        let ready_path = temp.path().join("ready");
        let restored_path = temp.path().join("restored");
        let mut owner_command = spawned_daemon_helper_command("terminal-owner");
        owner_command
            .env("ZEROCODE_TERMINAL_OWNER_READY_PATH", &ready_path)
            .env("ZEROCODE_TERMINAL_OWNER_RESTORED_PATH", &restored_path)
            .stdin(duplicate_stdio(slave_fd))
            .stdout(duplicate_stdio(slave_fd))
            .stderr(duplicate_stdio(slave_fd));
        let mut owner = owner_command.spawn().expect("spawn terminal owner");
        drop(owner_command);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_path.exists() {
            assert!(
                owner.try_wait().expect("poll terminal owner").is_none(),
                "terminal owner exited before activating the terminal"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "terminal owner did not activate the terminal"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut output = Vec::new();
        drain_pty_output(&mut master, &mut output);

        let active = terminal_attributes(slave_fd);
        assert_eq!(
            active.c_lflag & libc::ICANON,
            0,
            "terminal never entered raw mode"
        );
        let signal_result = unsafe { libc::kill(owner.id() as libc::pid_t, signal) };
        assert_eq!(
            signal_result,
            0,
            "send signal to terminal owner: {}",
            std::io::Error::last_os_error()
        );
        assert!(owner.wait().expect("wait for terminal owner").success());
        assert!(
            restored_path.exists(),
            "terminal owner exited before restore_terminal returned"
        );

        let restored = terminal_attributes(slave_fd);
        assert_terminal_attributes_equal(&original, &restored);
        drain_pty_output(&mut master, &mut output);
        unsafe { libc::close(slave_fd) };

        for sequence in [
            b"\x1b[?2004l".as_slice(),
            b"\x1b[?1006l".as_slice(),
            b"\x1b[?1000l".as_slice(),
            b"\x1b[?1049l".as_slice(),
        ] {
            assert!(
                output_contains(&output, sequence),
                "terminal teardown omitted {sequence:?}; output: {output:?}"
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sigterm_restores_terminal_state() {
        assert_signal_restores_terminal(libc::SIGTERM);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sigint_restores_terminal_state() {
        assert_signal_restores_terminal(libc::SIGINT);
    }

    #[test]
    fn spawned_daemon_readiness_allows_cold_start_window() {
        assert_eq!(SPAWNED_DAEMON_CONNECT_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    #[ignore = "subprocess helper for spawned-daemon lifecycle tests"]
    fn spawned_daemon_subprocess_helper() {
        match std::env::var("ZEROCODE_SPAWNED_DAEMON_HELPER").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(60)),
            Ok("sleep-short") => std::thread::sleep(Duration::from_secs(3)),
            Ok("stderr") => {
                eprint!("{}", "x".repeat(DAEMON_STDERR_LIMIT * 2));
                eprintln!("spawned-daemon-stderr-tail");
                std::process::exit(23);
            }
            Ok("unsafe-stderr") => {
                use std::io::Write;

                let mut stderr = std::io::stderr().lock();
                stderr
                    .write_all(&vec![0xff; DAEMON_STDERR_LIMIT * 2])
                    .expect("write invalid stderr");
                stderr
                    .write_all(b"\x1b[2J\0\runsafe-stderr-tail\r\n")
                    .expect("write control stderr");
                std::process::exit(23);
            }
            Ok("stderr-descendant") => {
                spawned_daemon_helper_command("sleep-short")
                    .spawn()
                    .expect("spawn stderr-inheriting descendant");
                eprintln!("stderr-descendant-parent-exit");
                std::process::exit(23);
            }
            Ok("exercise-inherited-stderr") => {
                let mut daemon =
                    SpawnedDaemon::spawn(spawned_daemon_helper_command("stderr-descendant"))
                        .expect("spawn stderr-descendant helper");
                let deadline = std::time::Instant::now() + Duration::from_secs(1);
                loop {
                    if daemon
                        .poll_exit()
                        .expect("poll stderr-descendant")
                        .is_some()
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "stderr-descendant helper did not exit"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            #[cfg(unix)]
            Ok("signal-owner") => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build signal runtime");
                runtime.block_on(async {
                    let mut termination_signals =
                        TerminationSignals::new().expect("install termination handlers");
                    let mut daemon = SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep"))
                        .expect("spawn owned daemon helper");
                    let pid_path = std::env::var_os("ZEROCODE_SIGNAL_OWNER_PID_PATH")
                        .expect("signal owner pid path");
                    std::fs::write(pid_path, daemon.id().to_string())
                        .expect("publish owned daemon pid");
                    termination_signals.recv().await;
                    cleanup_spawned_daemon_after_signal(&mut daemon)
                        .expect("clean up signalled daemon");
                });
            }
            #[cfg(unix)]
            Ok("insecure-tls-prompt-owner") => {
                let config_dir = std::env::var_os("ZEROCODE_INSECURE_PROMPT_CONFIG_DIR")
                    .expect("insecure prompt config dir");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build insecure prompt runtime");
                let _runtime_guard = runtime.enter();
                prepare_wss_termination_signals(
                    true,
                    std::path::Path::new(&config_dir),
                    "wss://insecure.example",
                )
                .expect("prepare WSS termination signals");
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Ok("terminal-owner") => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build terminal-owner runtime");
                runtime.block_on(async {
                    let mut termination_signals =
                        TerminationSignals::new().expect("install termination handlers");
                    let mut terminal =
                        config_manager::init_terminal().expect("initialize terminal");
                    TERMINAL_ACTIVE.store(true, Ordering::Relaxed);
                    let ready_path = std::env::var_os("ZEROCODE_TERMINAL_OWNER_READY_PATH")
                        .expect("terminal owner ready path");
                    std::fs::write(ready_path, b"ready").expect("publish terminal readiness");
                    termination_signals.recv().await;
                    TERMINAL_ACTIVE.store(false, Ordering::Relaxed);
                    config_manager::restore_terminal(&mut terminal).expect("restore terminal");
                    let restored_path = std::env::var_os("ZEROCODE_TERMINAL_OWNER_RESTORED_PATH")
                        .expect("terminal owner restored path");
                    std::fs::write(restored_path, b"restored")
                        .expect("publish terminal restoration");
                });
            }
            other => panic!("unexpected helper mode: {other:?}"),
        }
    }

    #[test]
    fn ephemeral_daemon_command_sets_selected_socket() {
        let mut cmd = std::process::Command::new("zeroclaw");
        configure_ephemeral_daemon_command(
            &mut cmd,
            std::path::Path::new("/tmp/zeroclaw-config"),
            std::path::Path::new("/tmp/zeroclaw.sock"),
        );

        assert_eq!(
            cmd.get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "daemon",
                "--ephemeral",
                "--config-dir",
                "/tmp/zeroclaw-config",
            ]
        );
        assert_eq!(
            cmd.get_envs()
                .find(|(name, _)| *name == OsStr::new("ZEROCLAW_SOCKET"))
                .and_then(|(_, value)| value),
            Some(OsStr::new("/tmp/zeroclaw.sock"))
        );
    }

    #[test]
    fn flag_connect_overrides_config_uri() {
        let cfg = WssSection {
            uri: Some("wss://config:1".to_string()),
            ..Default::default()
        };
        let got = resolve_direct_url(Some("wss://flag:2".to_string()), &cfg);
        assert_eq!(got.as_deref(), Some("wss://flag:2"));
    }

    #[test]
    fn config_uri_used_when_no_flag() {
        let cfg = WssSection {
            uri: Some("wss://config:1".to_string()),
            ..Default::default()
        };
        let got = resolve_direct_url(None, &cfg);
        assert_eq!(got.as_deref(), Some("wss://config:1"));
    }

    #[test]
    fn no_uri_anywhere_has_no_direct_address() {
        // With no direct address and no relay, the target resolves to the local
        // socket; here we assert the direct-address half of that decision.
        let cfg = WssSection::default();
        assert_eq!(resolve_direct_url(None, &cfg), None);
    }

    #[test]
    fn skip_verify_is_flag_or_config() {
        let mut cfg = WssSection::default();
        cfg.tls.skip_verify = true;
        assert!(resolve_skip_verify(false, &cfg));
        cfg.tls.skip_verify = false;
        assert!(resolve_skip_verify(true, &cfg)); // flag wins
        assert!(!resolve_skip_verify(false, &cfg)); // neither
    }

    #[test]
    fn relay_only_route_still_chooses_wss() {
        // A relay configured with no direct address must NOT collapse to the
        // local socket: the route is WSS-over-relay (direct_url stays None).
        let cfg = WssSection {
            relay_url: Some("relay.example:9783".to_string()),
            relay_node: Some("node-abc".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_direct_url(None, &cfg), None);
        assert!(cfg.relay_url.is_some() && cfg.relay_node.is_some());
    }

    #[test]
    fn explicit_relay_enroll_uses_relay_route() {
        let cli = Cli::parse_from([
            "zerocode",
            "--enroll",
            "--relay",
            "relay.example:9783",
            "--relay-node",
            "node-abc",
        ]);
        assert!(should_enroll_via_relay(&cli, &WssSection::default(), true));
    }

    #[test]
    fn explicit_enroll_host_keeps_direct_enrollment() {
        let cli = Cli::parse_from([
            "zerocode",
            "--enroll",
            "--relay",
            "relay.example:9783",
            "--relay-node",
            "node-abc",
            "--enroll-host",
            "daemon.example",
        ]);
        assert!(!should_enroll_via_relay(&cli, &WssSection::default(), true));
    }

    #[test]
    fn relay_only_config_enroll_uses_relay_route() {
        let cli = Cli::parse_from(["zerocode", "--enroll"]);
        let cfg = WssSection {
            relay_url: Some("relay.example:9783".to_string()),
            relay_node: Some("node-abc".to_string()),
            ..Default::default()
        };
        assert!(should_enroll_via_relay(&cli, &cfg, true));
    }

    #[test]
    fn initialize_timeout_is_a_terminal_connection_error() {
        let err = anyhow::Error::new(client::DaemonInitializeTimeout::new(Duration::from_secs(
            10,
        )));

        assert!(is_terminal_connection_error(&err));
    }
}

#[cfg(test)]
mod confirm_insecure_tls_tests {
    //! Tests for [`crate::confirm_insecure_tls_with`], the test-seam
    //! extracted from the original `confirm_insecure_tls(url)` so the
    //! input → choice mapping and prompt content can be asserted
    //! deterministically without touching `stdin` / `stderr`.
    //!
    //! Insecure-TLS acceptance criterion coverage:
    //! 1. "Insecure TLS cannot be accepted without explicit confirmation"
    //!    — the empty / `n` / junk / uppercase-`N` / default branches all
    //!    return [`InsecureTlsChoice::Abort`].
    //! 2. "Decline/abort paths leave no persisted insecure-TLS choice"
    //!    — covered behaviorally by `apply_insecure_tls_choice_tests`,
    //!    which executes the production choice -> side-effect seam
    //!    (`crate::apply_insecure_tls_choice`) for `Once`, `Always`, and
    //!    `Abort` and asserts persistence only happens for `Always`.
    //! 3. "Mode transition tests cover the quickstart/chat handoff" is
    //!    covered by the existing `connection_tests::flag_connect_*` /
    //!    `config_uri_*` / `skip_verify_*` tests; this issue does not
    //!    change `resolve_wss_target`'s contract.
    //! 4. "prompt persistence behavior needed to test those transitions
    //!    deterministically" is covered by the existing
    //!    `route_acked_membership` / `persist_wss_route_ack_dedups` /
    //!    `persist_wss_route_ack_preserves_other_sections` tests in
    //!    `crate::config` — this issue does not duplicate that coverage.

    use super::InsecureTlsChoice::{Abort, Always, Once};
    use super::*;
    use std::io::Cursor;

    /// Drive [`confirm_insecure_tls_with`] with a deterministic stdin
    /// buffer and a fresh output buffer, returning the operator's
    /// choice and the captured prompt text.
    fn run(input: &str, url: &str) -> (InsecureTlsChoice, String) {
        let mut output = Vec::new();
        let choice = confirm_insecure_tls_with(Cursor::new(input), &mut output, url)
            .expect("confirm_insecure_tls_with must succeed on plain stdin read");
        let stderr = String::from_utf8(output).expect("prompt must be valid UTF-8");
        (choice, stderr)
    }

    #[test]
    fn confirm_input_y_returns_once() {
        assert!(matches!(run("y\n", "wss://example.test:1").0, Once));
    }

    #[test]
    fn confirm_input_yes_returns_once() {
        assert!(matches!(run("yes\n", "wss://example.test:1").0, Once));
    }

    #[test]
    fn relay_cert_prompt_y_trusts_and_shows_fingerprint() {
        let mut output = Vec::new();
        let choice = confirm_relay_cert_with(
            Cursor::new("y\n"),
            &mut output,
            "relay.example:8443",
            "abcd1234ef",
        )
        .expect("relay prompt must read");
        assert_eq!(choice, RelayTrustChoice::Trust);
        let shown = String::from_utf8(output).expect("prompt is UTF-8");
        assert!(
            shown.contains("abcd1234ef"),
            "fingerprint must be shown: {shown}"
        );
        assert!(shown.contains("relay.example:8443"));
    }

    #[test]
    fn relay_cert_prompt_default_and_no_abort() {
        let mut out = Vec::new();
        assert_eq!(
            confirm_relay_cert_with(Cursor::new("\n"), &mut out, "r:1", "fp").unwrap(),
            RelayTrustChoice::Abort
        );
        assert_eq!(
            confirm_relay_cert_with(Cursor::new("n\n"), &mut out, "r:1", "fp").unwrap(),
            RelayTrustChoice::Abort
        );
    }

    #[test]
    fn confirm_input_a_returns_always() {
        assert!(matches!(run("a\n", "wss://example.test:1").0, Always));
    }

    #[test]
    fn confirm_input_always_returns_always() {
        assert!(matches!(run("always\n", "wss://example.test:1").0, Always));
    }

    #[test]
    fn confirm_input_n_returns_abort() {
        assert!(matches!(run("n\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_input_empty_returns_abort() {
        // Acceptance: insecure TLS cannot be accepted without explicit
        // confirmation. An empty stdin (e.g. operator hits enter without
        // typing) must default-decline.
        assert!(matches!(run("\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_input_junk_returns_abort() {
        // Acceptance: unknown input must default to the safe Abort
        // branch — only `y` / `yes` / `a` / `always` may opt into
        // verification-disabled transport.
        assert!(matches!(run("xyz\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_input_uppercase_lowercases_before_match() {
        // The match arm uses `to_ascii_lowercase()` so case variations
        // resolve identically. This is the seam's contract; pin both
        // "Once" and "Always" branches to defend against an
        // accidental case-sensitive refactor.
        assert!(matches!(run("Y\n", "wss://example.test:1").0, Once));
        assert!(matches!(run("YES\n", "wss://example.test:1").0, Once));
        assert!(matches!(run("ALWAYS\n", "wss://example.test:1").0, Always));
        // Uppercase `N` and `NO` must still resolve to Abort — they
        // are not in the affirmative set.
        assert!(matches!(run("N\n", "wss://example.test:1").0, Abort));
        assert!(matches!(run("NO\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_prompt_writes_url_and_choice_menu_to_writer() {
        // The operator must see (a) which URL they are accepting
        // insecure-TLS for, and (b) the `[y/a/N]` choice menu, before
        // any answer is read. Capture the prompt text and pin both
        // invariants so a future refactor cannot silently truncate the
        // warning or the menu.
        let url = "wss://insecure-host.example:8443";
        let (_, stderr) = run("n\n", url);
        assert!(
            stderr.contains(url),
            "stderr prompt must contain the URL being confirmed; got: {stderr}"
        );
        assert!(
            stderr.contains("[y/a/N]"),
            "stderr prompt must show the y/a/N choice menu; got: {stderr}"
        );
        assert!(
            stderr.contains("WARNING"),
            "stderr prompt must lead with a WARNING banner so the \
             operator does not skim past an insecure-TLS confirmation; \
             got: {stderr}"
        );
    }
}

#[cfg(test)]
mod apply_insecure_tls_choice_tests {
    //! Behavior-level tests for [`crate::apply_insecure_tls_choice`], the
    //! test seam extracted from the `Once` / `Always` / `Abort` match
    //! that used to live inline in `run()`. These tests execute the
    //! production choice -> side-effect path directly (no source-text
    //! inspection) against a temporary config directory:
    //!
    //! - `Once` must not persist a route acknowledgement.
    //! - `Always` must persist the confirmed route, and only that route.
    //! - `Abort` must return the exact production error and persist
    //!   nothing.
    //! - `Always` must not disturb unrelated, pre-existing config
    //!   sections on disk.

    use super::*;

    const URL: &str = "wss://example.invalid:8443/";

    #[test]
    fn once_does_not_persist() {
        let dir = tempfile::tempdir().unwrap();
        apply_insecure_tls_choice(InsecureTlsChoice::Once, dir.path(), URL)
            .expect("Once must not error");
        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            !cfg.connection.wss.tls.route_acked(URL),
            "Once must leave no route acknowledgement behind"
        );
    }

    #[test]
    fn always_persists_exact_route() {
        let dir = tempfile::tempdir().unwrap();
        apply_insecure_tls_choice(InsecureTlsChoice::Always, dir.path(), URL)
            .expect("Always must not error");
        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            cfg.connection.wss.tls.route_acked(URL),
            "Always must persist the confirmed route"
        );
        assert_eq!(
            cfg.connection.wss.tls.skip_verify_routes,
            vec![URL.to_string()],
            "Always must store exactly the confirmed route, unmutated"
        );
    }

    #[test]
    fn abort_returns_error_and_persists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply_insecure_tls_choice(InsecureTlsChoice::Abort, dir.path(), URL)
            .expect_err("Abort must return an error");
        assert!(
            err.to_string()
                .contains("aborted: insecure TLS connection not confirmed"),
            "unexpected error message: {err}"
        );
        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            !cfg.connection.wss.tls.route_acked(URL),
            "Abort must leave no route acknowledgement behind"
        );
    }

    #[test]
    fn always_preserves_unrelated_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config::config_path(dir.path()),
            "[theme]\nname = \"nord\"\n\n[future]\nkeep = true\n",
        )
        .unwrap();

        apply_insecure_tls_choice(InsecureTlsChoice::Always, dir.path(), URL)
            .expect("Always must not error");

        let on_disk = std::fs::read_to_string(config::config_path(dir.path())).unwrap();
        let doc: toml::Table = toml::from_str(&on_disk).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));

        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            cfg.connection.wss.tls.route_acked(URL),
            "Always must persist the confirmed route alongside unrelated sections"
        );
    }
}

#[cfg(test)]
mod credential_recovery_order_tests {
    //! Startup-ORDER regressions for [`crate::recover_then_read_credentials`].
    //!
    //! An interrupted credential publication cannot be produced by cutting the
    //! power in a test, so these build the real post-crash state with the
    //! publication's own steps (`enroll::interrupt_publication_for_test`) and
    //! then assert what startup reads. Each test first shows what a read taken
    //! BEFORE recovery would have reported, so the assertions prove the order
    //! matters rather than restating the recovered state.

    use super::*;

    fn profile_json(relay_url: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "device_id": "dev_1",
            "not_after": 0,
            "relay": {
                "relay_url": relay_url,
                "node_id": "node-1",
                "relay_cert_pin": "",
            },
        }))
        .unwrap()
    }

    fn publish_old_generation(tls: &std::path::Path, relay_url: &str) {
        std::fs::create_dir_all(tls).unwrap();
        std::fs::write(tls.join("client.crt"), b"OLD-CERT").unwrap();
        std::fs::write(tls.join("ca.crt"), b"OLD-CA").unwrap();
        std::fs::write(tls.join("client.key"), b"OLD-KEY").unwrap();
        std::fs::write(tls.join("profile.json"), profile_json(relay_url)).unwrap();
    }

    /// A publication interrupted after its FIRST rename leaves the new
    /// certificate beside the old profile. Reading before recovery hands the
    /// connect path the OLD relay coordinates, and pairs a new certificate with
    /// the old key.
    #[test]
    fn a_first_rename_interruption_does_not_yield_stale_relay_coordinates() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls, "old-relay.invalid:443");
        let new_profile = profile_json("new-relay.invalid:443");
        let payloads: [&[u8]; 4] = [b"NEW-CERT", b"NEW-CA", b"NEW-KEY", new_profile.as_bytes()];
        enroll::interrupt_publication_for_test(dir.path(), payloads, 1).unwrap();

        // What an unordered startup would have read.
        assert_eq!(
            enroll::cached_profile(dir.path()).unwrap().relay.relay_url,
            "old-relay.invalid:443",
            "the interrupted state must still carry the stale coordinates"
        );

        let credentials = recover_then_read_credentials(dir.path(), None, "").unwrap();

        assert_eq!(
            credentials.profile.unwrap().relay.relay_url,
            "new-relay.invalid:443",
            "startup must read the recovered generation, not the interrupted one"
        );
        assert_eq!(std::fs::read(tls.join("client.key")).unwrap(), b"NEW-KEY");
        assert!(!credentials.certless);
    }

    /// A first enrollment interrupted before any rename has no published
    /// certificate yet. Read before recovery it looks certless, which re-runs
    /// enrollment and discards the certificate the daemon already issued.
    #[test]
    fn an_interrupted_first_enrollment_is_not_reported_as_certless() {
        let dir = tempfile::tempdir().unwrap();
        let new_profile = profile_json("new-relay.invalid:443");
        let payloads: [&[u8]; 4] = [b"NEW-CERT", b"NEW-CA", b"NEW-KEY", new_profile.as_bytes()];
        enroll::interrupt_publication_for_test(dir.path(), payloads, 0).unwrap();

        // What an unordered startup would have read.
        assert!(enroll::is_certless(dir.path(), None, ""));
        assert!(enroll::cached_profile(dir.path()).is_none());

        let credentials = recover_then_read_credentials(dir.path(), None, "").unwrap();

        assert!(
            !credentials.certless,
            "recovery must publish the issued certificate before enrollment is considered"
        );
        assert_eq!(
            credentials.profile.unwrap().relay.relay_url,
            "new-relay.invalid:443"
        );
    }

    /// Recovery that cannot reassemble the generation must stop the run, not
    /// warn and continue into relay resolution and TLS path derivation with a
    /// mixed set.
    #[test]
    fn an_unrecoverable_publication_stops_startup() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls, "old-relay.invalid:443");
        let new_profile = profile_json("new-relay.invalid:443");
        let payloads: [&[u8]; 4] = [b"NEW-CERT", b"NEW-CA", b"NEW-KEY", new_profile.as_bytes()];
        enroll::interrupt_publication_for_test(dir.path(), payloads, 1).unwrap();
        std::fs::remove_file(tls.join(".client.key.tmp")).unwrap();

        let err = recover_then_read_credentials(dir.path(), None, "").unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("interrupted credential publication"),
            "got: {err}"
        );
        assert!(err.contains("client.key"), "got: {err}");
        assert!(
            tls.join(".publish.pending").exists(),
            "an incomplete generation must keep its marker for the next run"
        );
    }

    /// A publication whose marker did not become durable leaves a mixed set with
    /// nothing on disk pointing at it. Recovery sees no marker and has nothing to
    /// do, so startup must refuse on the recorded generation instead of dialling
    /// with a new certificate and the old key.
    #[test]
    fn a_mixed_set_without_a_marker_stops_startup() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        publish_old_generation(&tls, "old-relay.invalid:443");
        // The first run adopts and records the generation it finds.
        recover_then_read_credentials(dir.path(), None, "")
            .expect("a pre-record credential set must still start");

        let new_profile = profile_json("new-relay.invalid:443");
        let payloads: [&[u8]; 4] = [b"NEW-CERT", b"NEW-CA", b"NEW-KEY", new_profile.as_bytes()];
        enroll::interrupt_publication_for_test(dir.path(), payloads, 1).unwrap();
        std::fs::remove_file(tls.join(".publish.pending")).unwrap();

        let err = format!(
            "{:#}",
            recover_then_read_credentials(dir.path(), None, "")
                .expect_err("startup must refuse a mixed credential set")
        );
        assert!(
            err.contains("published credential set is inconsistent"),
            "got: {err}"
        );
        assert!(err.contains("client.crt"), "got: {err}");
    }
}
