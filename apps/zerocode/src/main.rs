// `apps/zerocode` is a standalone TUI client, not daemon-path code.
// It speaks JSON-RPC to whatever ZeroClaw daemon is at the configured
// address; the daemon owns attribution, the TUI owns its session id.
// Bare `tokio::spawn` is the right primitive here — the workspace-wide
// `zeroclaw_spawn::spawn!` rule is daemon-path only (see
// `clippy.toml`'s commentary; this matches the `robot-kit/src/safety.rs`
// exemption pattern).
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use clap::Parser;

mod acp;
mod app;
mod attachment;
mod chat;
mod client;
mod clipboard;
mod color_depth;
mod config;
mod config_manager;
mod dashboard;
mod diff;
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
mod quickstart_pane;
mod theme;
mod turn_status;
mod widgets;
mod wire;
mod zerocode_pane;

const DAEMON_CONNECT_INTERVAL: Duration = Duration::from_millis(50);
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Set to `true` once the alternate screen is active so signal/panic
/// handlers know they need to restore the terminal before exiting.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

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
        .or_else(|| uri.strip_prefix("ws://"))
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

/// Best-effort terminal restoration used by the panic hook and SIGTERM
/// handler.  Errors are intentionally ignored — we're already crashing.
fn force_restore_terminal() {
    if TERMINAL_ACTIVE.load(Ordering::Relaxed) {
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

fn confirm_insecure_tls(url: &str) -> anyhow::Result<InsecureTlsChoice> {
    use std::io::Write as _;
    eprintln!(
        "\nWARNING: --tls-skip-verify DISABLES TLS certificate verification for\n\
         {url}\nThis connection is UNSAFE on untrusted networks (susceptible to\n\
         man-in-the-middle). Only continue on a trusted network against a\n\
         self-signed cert you control.\n\n\
         You are accepting an UNVERIFIED route, not a trusted peer.\n\
         [y] yes, connect once   [a] always (remember this route)   [N] no, abort"
    );
    eprint!("Continue with verification disabled? [y/a/N] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(InsecureTlsChoice::Once),
        "a" | "always" => Ok(InsecureTlsChoice::Always),
        _ => Ok(InsecureTlsChoice::Abort),
    }
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
        let wss_intended = cli.connect.is_some()
            || cfg_wss.uri.is_some()
            || cli.relay.is_some()
            || cfg_wss.relay_url.is_some();
        let certless = enroll::is_certless(
            &config_dir,
            cli.tls_client_cert.as_deref(),
            &cfg_wss.tls.client_cert_path,
        );
        let auto =
            wss_intended && certless && cli.connect.is_some() && std::io::stderr().is_terminal();
        if cli.enroll || auto {
            let host = cli
                .enroll_host
                .clone()
                .or_else(|| enroll_host_from(cli.connect.as_deref()))
                .or_else(|| enroll_host_from(cfg_wss.uri.as_deref()))
                .ok_or_else(|| {
                    anyhow::Error::msg(
                        "enrollment needs a host: pass --enroll-host or --connect wss://<host>:<port>",
                    )
                })?;
            let port = cli.enroll_port.unwrap_or(enroll::DEFAULT_ENROLL_PORT);
            enroll::enroll(&host, port, &config_dir).await?;
        }
    }

    let target = {
        let cfg_wss = &loaded_config.connection.wss;
        let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
        // The cached enrollment profile supplies the relay coordinates so a bare
        // `zerocode` after enrollment still reaches the daemon through its relay.
        let cached = enroll::cached_profile(&config_dir);
        let cached_relay = cached.as_ref().map(|p| &p.relay);

        // Direct daemon address (CLI overrides config). `None` => relay-only.
        let direct_url = resolve_direct_url(cli.connect.clone(), cfg_wss);
        let skip_verify = resolve_skip_verify(cli.tls_skip_verify, cfg_wss);

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

        // Relay outer-leaf pin: --relay-pin -> the enrollment-delivered pin -> a
        // previously TOFU'd pin. TOFU persists here for the next run.
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

        let relay = match (relay_addr, relay_node) {
            (Some(relay_addr), Some(node_id)) => {
                // Default the relay's expected cert name to its host:port host.
                let relay_host = cli.relay_host.clone().unwrap_or_else(|| {
                    relay_addr
                        .rsplit_once(':')
                        .map(|(h, _)| h.to_string())
                        .unwrap_or_else(|| relay_addr.clone())
                });
                Some(client::RelayDial {
                    relay_addr,
                    relay_host,
                    node_id,
                    relay_ca_path: cli.relay_ca.clone(),
                    relay_insecure: cli.relay_insecure,
                    relay_pin: relay_pin.clone(),
                    relay_tofu: cli.relay_tofu,
                    pin_store: Some(pin_store.clone()),
                    outer_client_cert: cli.relay_client_cert.clone(),
                    outer_client_key: cli.relay_client_key.clone(),
                })
            }
            (None, None) => None,
            _ => {
                return Err(anyhow::Error::msg(
                    "relay routing needs both a relay address and a node-id \
                     (--relay + --relay-node, or wss.relay_url + wss.relay_node)",
                ));
            }
        };

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

    // Initial connection (before the terminal is initialized).
    // `owns_ephemeral` records whether THIS process spawned the daemon
    // (initial connect failed → we started one). Only an owned ephemeral
    // daemon may be respawned on disconnect, and then exactly once.
    let mut owns_ephemeral = false;
    let (rpc, initial_leg) = match &target {
        ConnectTarget::LocalSocket(socket) => {
            match client::RpcClient::connect(socket, None, None).await {
                Ok(c) => (c, ActiveLeg::Local),
                Err(e) if is_daemon_version_mismatch(&e) => return Err(e),
                Err(_) => {
                    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
                    spawn_ephemeral_daemon(&config_dir)?;
                    owns_ephemeral = true;
                    (await_daemon_ready(socket).await?, ActiveLeg::Local)
                }
            }
        }
        ConnectTarget::Wss(route) => {
            if route.tls.skip_verify {
                let ack_key = route.ack_key();
                if !loaded_config.connection.wss.tls.route_acked(&ack_key) {
                    match confirm_insecure_tls(&ack_key)? {
                        InsecureTlsChoice::Once => {}
                        InsecureTlsChoice::Always => {
                            config::persist_wss_route_ack(&local_config_dir, &ack_key)?;
                        }
                        InsecureTlsChoice::Abort => {
                            anyhow::bail!("aborted: insecure TLS connection not confirmed");
                        }
                    }
                }
            }
            match route.connect_preferred(None, None).await {
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
    )
    .await;

    TERMINAL_ACTIVE.store(false, Ordering::Relaxed);
    config_manager::restore_terminal(&mut term)?;
    result
}

/// Runs the TUI under a SIGTERM handler so the terminal is restored on
/// signal instead of dying mid-draw. `app::run` owns the full session
/// lifecycle — including in-loop reconnection and recovery — and returns
/// only when the user quits.
async fn run_until_exit(
    rpc: Arc<client::RpcClient>,
    term: &mut config_manager::Term,
    target: &ConnectTarget,
    config_dir: &std::path::Path,
    owns_ephemeral: bool,
    initial_leg: ActiveLeg,
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
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            r = app::run(rpc, term, &label, insecure_tls, reconnect_state, config_dir, target, owns_ephemeral, initial_leg) => r.map(|_| ()),
            _ = sigterm.recv() => Ok(()),
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

pub(crate) fn spawn_ephemeral_daemon(config_dir: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("zeroclaw")))
        .unwrap_or_else(|| PathBuf::from("zeroclaw"));

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .arg("--ephemeral")
        .arg("--config-dir")
        .arg(config_dir);

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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn()
        .map_err(|e| anyhow::Error::msg(format!("failed to spawn daemon: {e}")))?;

    Ok(())
}

async fn await_daemon_ready(socket: &std::path::Path) -> anyhow::Result<client::RpcClient> {
    let deadline = tokio::time::Instant::now() + DAEMON_CONNECT_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon did not become ready within {}s (socket: {})",
                DAEMON_CONNECT_TIMEOUT.as_secs(),
                socket.display(),
            );
        }
        match client::RpcClient::connect(socket, None, None).await {
            Ok(c) => return Ok(c),
            Err(e) if is_daemon_version_mismatch(&e) => return Err(e),
            Err(_) => tokio::time::sleep(DAEMON_CONNECT_INTERVAL).await,
        }
    }
}

fn is_daemon_version_mismatch(err: &anyhow::Error) -> bool {
    err.downcast_ref::<client::DaemonVersionMismatch>()
        .is_some()
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::config::WssSection;

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
}
