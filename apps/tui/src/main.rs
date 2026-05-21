use std::process::ExitCode;

mod client;
mod config_manager;
mod theme;
mod widgets;

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
    let socket = client::resolve_socket_path()?;

    // Try connecting to an existing daemon first.
    let rpc = match client::RpcClient::connect(&socket).await {
        Ok(c) => c,
        Err(_) => {
            // No daemon running — spawn one in ephemeral mode.
            spawn_ephemeral_daemon(&socket).await?;
            client::RpcClient::connect(&socket).await?
        }
    };

    config_manager::run(&rpc).await
}

async fn spawn_ephemeral_daemon(socket: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("zeroclaw")))
        .unwrap_or_else(|| std::path::PathBuf::from("zeroclaw"));

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("daemon").arg("--ephemeral");

    if let Ok(dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        cmd.arg("--config-dir").arg(dir);
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn daemon: {e}"))?;

    // Wait for socket to appear.
    for _ in 0..100 {
        if socket.exists() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    anyhow::bail!(
        "daemon did not start (socket never appeared at {})",
        socket.display()
    )
}
