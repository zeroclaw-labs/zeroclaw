use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

mod acp;
mod app;
mod chat;
mod client;
mod config_manager;
mod dashboard;
mod logs;
mod mouse;
mod theme;
mod widgets;

const DAEMON_CONNECT_INTERVAL: Duration = Duration::from_millis(50);
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zeroclaw-tui: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
    let socket = client::resolve_socket_path(&config_dir)?;

    // Initial connection (before the terminal is initialized).
    let mut rpc = match client::RpcClient::connect(&socket).await {
        Ok(c) => c,
        Err(_) => {
            spawn_ephemeral_daemon(&config_dir)?;
            await_daemon_ready(&socket).await?
        }
    };

    let mut term = config_manager::init_terminal()?;
    let result = run_with_reconnect(&mut rpc, &mut term, &socket).await;
    config_manager::restore_terminal(&mut term)?;
    result
}

async fn run_with_reconnect(
    rpc: &mut client::RpcClient,
    term: &mut config_manager::Term,
    socket: &std::path::Path,
) -> anyhow::Result<()> {
    loop {
        let should_reconnect = app::run(rpc, term).await?;
        if !should_reconnect {
            return Ok(());
        }
        // Retry connecting to the existing socket. We do NOT spawn a new
        // daemon here — multiple TUIs reconnecting simultaneously would
        // each spawn their own, causing a stampede. The daemon is managed
        // externally (service manager, manual restart, or the initial
        // startup path in run()).
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Ok(c) = client::RpcClient::connect(socket).await {
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
        .map_err(|e| anyhow::anyhow!("failed to spawn daemon: {e}"))?;

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
        match client::RpcClient::connect(socket).await {
            Ok(c) => return Ok(c),
            Err(_) => tokio::time::sleep(DAEMON_CONNECT_INTERVAL).await,
        }
    }
}
