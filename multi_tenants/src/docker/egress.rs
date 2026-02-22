use super::DockerManager;
use anyhow::{bail, Result};

/// Start the egress proxy container.
/// Connected to BOTH internal and external networks (dual-homed).
/// Tenant containers route API traffic through this proxy.
pub fn start_egress_proxy(
    internal_network: &str,
    external_network: &str,
    allowed_domains: &[&str],
) -> Result<()> {
    let container_name = "zcplatform-egress-proxy";

    // Check if already running
    let out = DockerManager::exec(&["inspect", "-f", "{{.State.Running}}", container_name])?;
    if out.success && out.stdout.trim() == "true" {
        return Ok(());
    }

    // Remove stale container
    let _ = DockerManager::exec(&["rm", "-f", container_name]);

    // Build allowed domains connect list (domain:443 per entry, comma-separated)
    let connect_list: String = allowed_domains
        .iter()
        .map(|d| format!("{}:443", d))
        .collect::<Vec<_>>()
        .join(",");

    let env_connect = format!("ALLOWED_CONNECT={}", connect_list);

    let out = DockerManager::exec(&[
        "run",
        "-d",
        "--name",
        container_name,
        "--network",
        internal_network,
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--read-only",
        "--memory=64m",
        "--cpus=0.5",
        "--tmpfs",
        "/tmp:size=10m",
        "--tmpfs",
        "/run:size=1m",
        "-e",
        &env_connect,
        "zcplatform-egress:latest",
    ])?;

    if !out.success {
        bail!("egress proxy start failed: {}", out.stderr.trim());
    }

    // Connect to external network (dual-homed)
    let out = DockerManager::exec(&["network", "connect", external_network, container_name])?;
    if !out.success {
        bail!("egress proxy network connect failed: {}", out.stderr.trim());
    }

    tracing::info!(
        "Egress proxy started with {} allowed domains",
        allowed_domains.len()
    );
    Ok(())
}

/// Stop and remove egress proxy.
pub fn stop_egress_proxy() -> Result<()> {
    let _ = DockerManager::exec(&["rm", "-f", "zcplatform-egress-proxy"]);
    Ok(())
}
