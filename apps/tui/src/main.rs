use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;

mod acp;
mod app;
mod attachment;
mod chat;
mod client;
mod clipboard;
mod config_manager;
mod dashboard;
mod diff;
mod file_explorer;
mod input_bar;
mod logs;
mod mouse;
mod onboard_pane;
mod theme;
mod widgets;

const DAEMON_CONNECT_INTERVAL: Duration = Duration::from_millis(50);
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Set to `true` once the alternate screen is active so signal/panic
/// handlers know they need to restore the terminal before exiting.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(
    name = "zeroclaw-tui",
    about = "Interactive TUI config manager for ZeroClaw"
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

/// Where the TUI should connect.
enum ConnectTarget {
    UnixSocket(PathBuf),
    Wss { url: String, skip_verify: bool },
}

impl ConnectTarget {
    /// Human-readable label for the dashboard Status box.
    fn label(&self) -> String {
        match self {
            Self::UnixSocket(p) => format!("unix:{}", p.display()),
            Self::Wss { url, .. } => url.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    install_panic_hook();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zeroclaw-tui: {e:#}");
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

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let target = if let Some(url) = cli.connect {
        ConnectTarget::Wss {
            url,
            skip_verify: cli.tls_skip_verify,
        }
    } else {
        let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
        let socket = client::resolve_socket_path(&config_dir)?;
        ConnectTarget::UnixSocket(socket)
    };

    // Initial connection (before the terminal is initialized).
    let mut rpc = match &target {
        ConnectTarget::UnixSocket(socket) => {
            match client::RpcClient::connect(socket, None, None).await {
                Ok(c) => c,
                Err(_) => {
                    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
                    spawn_ephemeral_daemon(&config_dir)?;
                    await_daemon_ready(socket).await?
                }
            }
        }
        ConnectTarget::Wss { url, skip_verify } => {
            client::RpcClient::connect_wss(url, None, None, *skip_verify).await?
        }
    };

    let mut term = config_manager::init_terminal()?;
    TERMINAL_ACTIVE.store(true, Ordering::Relaxed);

    let result = run_until_exit(&mut rpc, &mut term, &target).await;

    TERMINAL_ACTIVE.store(false, Ordering::Relaxed);
    config_manager::restore_terminal(&mut term)?;
    result
}

/// Wraps the reconnect loop with a SIGTERM handler so the TUI exits
/// cleanly (terminal restored) instead of dying mid-draw.
async fn run_until_exit(
    rpc: &mut client::RpcClient,
    term: &mut config_manager::Term,
    target: &ConnectTarget,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            r = run_with_reconnect(rpc, term, target) => r,
            _ = sigterm.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        run_with_reconnect(rpc, term, target).await
    }
}

async fn run_with_reconnect(
    rpc: &mut client::RpcClient,
    term: &mut config_manager::Term,
    target: &ConnectTarget,
) -> anyhow::Result<()> {
    loop {
        let label = target.label();
        let should_reconnect = match app::run(rpc, term, &label, None).await {
            Ok(reconnect) => reconnect,
            Err(_) if rpc.is_disconnected() => {
                // RPC error caused by a dead connection — treat as
                // disconnect and enter the reconnect loop instead of
                // propagating a fatal error.
                true
            }
            Err(e) => return Err(e),
        };
        if !should_reconnect {
            return Ok(());
        }
        // Preserve TUI identity across reconnects so the daemon can
        // reclaim the same UID via HMAC signature verification.
        let prev_id = rpc.tui_id().map(String::from);
        let prev_sig = rpc.tui_sig().map(String::from);
        // Retry connecting. We do NOT spawn a new daemon here — multiple
        // TUIs reconnecting simultaneously would each spawn their own,
        // causing a stampede. The daemon is managed externally (service
        // manager, manual restart, or the initial startup path in run()).
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let result = match target {
                ConnectTarget::UnixSocket(socket) => {
                    client::RpcClient::connect(socket, prev_id.as_deref(), prev_sig.as_deref())
                        .await
                }
                ConnectTarget::Wss { url, skip_verify } => {
                    client::RpcClient::connect_wss(
                        url,
                        prev_id.as_deref(),
                        prev_sig.as_deref(),
                        *skip_verify,
                    )
                    .await
                }
            };
            if let Ok(c) = result {
                *rpc = c;
                break;
            }
        }
    }
}

fn spawn_ephemeral_daemon(config_dir: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("zeroclaw")))
        .unwrap_or_else(|| PathBuf::from("zeroclaw"));

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("daemon")
        .arg("--ephemeral")
        .arg("--config-dir")
        .arg(config_dir);

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
