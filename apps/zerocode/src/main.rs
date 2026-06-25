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
mod file_explorer;
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
}

/// Map an empty path string to `None`.
fn opt_path(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Where zerocode should connect.
pub(crate) enum ConnectTarget {
    LocalSocket(PathBuf),
    Wss { url: String, tls: client::ClientTls },
}

impl ConnectTarget {
    /// Human-readable label for the dashboard Status box.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::LocalSocket(p) => format!("local:{}", p.display()),
            Self::Wss { url, .. } => url.clone(),
        }
    }

    pub(crate) fn insecure_tls(&self) -> bool {
        matches!(self, Self::Wss { tls, .. } if tls.skip_verify)
    }

    /// Connect to this target, reclaiming a prior TUI identity when
    /// `prev_id`/`prev_sig` are supplied. Single source of truth for the
    /// per-transport connect call — used by initial startup and in-loop
    /// reconnection alike.
    pub(crate) async fn connect(
        &self,
        prev_id: Option<&str>,
        prev_sig: Option<&str>,
    ) -> anyhow::Result<client::RpcClient> {
        match self {
            Self::LocalSocket(socket) => {
                client::RpcClient::connect(socket, prev_id, prev_sig).await
            }
            Self::Wss { url, tls } => {
                client::RpcClient::connect_wss(url, prev_id, prev_sig, tls).await
            }
        }
    }
}

fn resolve_wss_target(
    cli_connect: Option<String>,
    cli_skip_verify: bool,
    cfg_wss: &config::WssSection,
) -> Option<(String, bool)> {
    let uri = cli_connect.or_else(|| cfg_wss.uri.clone())?;
    let skip_verify = cli_skip_verify || cfg_wss.tls.skip_verify;
    Some((uri, skip_verify))
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

    let target = {
        let cfg_wss = &loaded_config.connection.wss;
        if let Some((uri, skip_verify)) =
            resolve_wss_target(cli.connect.clone(), cli.tls_skip_verify, cfg_wss)
        {
            // CLI flags override config for the relay route and the mutual-TLS
            // material.
            let relay_addr = cli.relay.clone().or_else(|| cfg_wss.relay_url.clone());
            let relay_node = cli
                .relay_node
                .clone()
                .or_else(|| cfg_wss.relay_node.clone());
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
            let tls = client::ClientTls {
                skip_verify,
                ca_cert_path: cli
                    .tls_ca_cert
                    .clone()
                    .or_else(|| opt_path(&cfg_wss.tls.ca_cert_path)),
                client_cert_path: cli
                    .tls_client_cert
                    .clone()
                    .or_else(|| opt_path(&cfg_wss.tls.client_cert_path)),
                client_key_path: cli
                    .tls_client_key
                    .clone()
                    .or_else(|| opt_path(&cfg_wss.tls.client_key_path)),
                relay,
            };
            ConnectTarget::Wss { url: uri, tls }
        } else {
            let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
            let socket = client::resolve_socket_path(&config_dir)?;
            ConnectTarget::LocalSocket(socket)
        }
    };

    // Initial connection (before the terminal is initialized).
    // `owns_ephemeral` records whether THIS process spawned the daemon
    // (initial connect failed → we started one). Only an owned ephemeral
    // daemon may be respawned on disconnect, and then exactly once.
    let mut owns_ephemeral = false;
    let rpc = match &target {
        ConnectTarget::LocalSocket(socket) => {
            match client::RpcClient::connect(socket, None, None).await {
                Ok(c) => c,
                Err(e) if is_daemon_version_mismatch(&e) => return Err(e),
                Err(_) => {
                    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
                    spawn_ephemeral_daemon(&config_dir)?;
                    owns_ephemeral = true;
                    await_daemon_ready(socket).await?
                }
            }
        }
        ConnectTarget::Wss { url, tls } => {
            if tls.skip_verify && !loaded_config.connection.wss.tls.route_acked(url) {
                match confirm_insecure_tls(url)? {
                    InsecureTlsChoice::Once => {}
                    InsecureTlsChoice::Always => {
                        config::persist_wss_route_ack(&local_config_dir, url)?;
                    }
                    InsecureTlsChoice::Abort => {
                        anyhow::bail!("aborted: insecure TLS connection not confirmed");
                    }
                }
            }
            client::RpcClient::connect_wss(url, None, None, tls).await?
        }
    };

    let mut term = config_manager::init_terminal()?;
    TERMINAL_ACTIVE.store(true, Ordering::Relaxed);

    let result = run_until_exit(
        Arc::new(rpc),
        &mut term,
        &target,
        &local_config_dir,
        owns_ephemeral,
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
            r = app::run(rpc, term, &label, insecure_tls, reconnect_state, config_dir, target, owns_ephemeral) => r.map(|_| ()),
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
        let got = resolve_wss_target(Some("wss://flag:2".to_string()), false, &cfg);
        assert_eq!(got, Some(("wss://flag:2".to_string(), false)));
    }

    #[test]
    fn config_uri_used_when_no_flag() {
        let cfg = WssSection {
            uri: Some("wss://config:1".to_string()),
            ..Default::default()
        };
        let got = resolve_wss_target(None, false, &cfg);
        assert_eq!(got, Some(("wss://config:1".to_string(), false)));
    }

    #[test]
    fn no_uri_anywhere_is_local_socket() {
        let cfg = WssSection::default();
        assert_eq!(resolve_wss_target(None, false, &cfg), None);
    }

    #[test]
    fn skip_verify_is_flag_or_config() {
        let mut cfg = WssSection {
            uri: Some("wss://h:1".to_string()),
            ..Default::default()
        };
        cfg.tls.skip_verify = true;
        assert_eq!(
            resolve_wss_target(None, false, &cfg),
            Some(("wss://h:1".to_string(), true))
        );
        cfg.tls.skip_verify = false;
        assert_eq!(
            resolve_wss_target(None, true, &cfg),
            Some(("wss://h:1".to_string(), true))
        );
        assert_eq!(
            resolve_wss_target(None, false, &cfg),
            Some(("wss://h:1".to_string(), false))
        );
    }
}
