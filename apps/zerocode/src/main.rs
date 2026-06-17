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
}

/// Where zerocode should connect.
pub(crate) enum ConnectTarget {
    LocalSocket(PathBuf),
    Wss { url: String, skip_verify: bool },
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
        matches!(
            self,
            Self::Wss {
                skip_verify: true,
                ..
            }
        )
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
            Self::Wss { url, skip_verify } => {
                client::RpcClient::connect_wss(url, prev_id, prev_sig, *skip_verify).await
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
            eprintln!("zerocode: {e:#}");
            ExitCode::FAILURE
        }
    }
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
            ConnectTarget::Wss {
                url: uri,
                skip_verify,
            }
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
                Err(_) => {
                    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
                    spawn_ephemeral_daemon(&config_dir)?;
                    owns_ephemeral = true;
                    await_daemon_ready(socket).await?
                }
            }
        }
        ConnectTarget::Wss { url, skip_verify } => {
            if *skip_verify && !loaded_config.connection.wss.tls.route_acked(url) {
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
            client::RpcClient::connect_wss(url, None, None, *skip_verify).await?
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
            Err(_) => tokio::time::sleep(DAEMON_CONNECT_INTERVAL).await,
        }
    }
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
