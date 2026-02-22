use super::DockerManager;
use anyhow::{bail, Result};

/// Create the internal Docker network if it doesn't exist.
/// Uses --internal to block outbound internet access.
pub fn ensure_network(name: &str) -> Result<()> {
    let out = DockerManager::exec(&["network", "inspect", name])?;
    if out.success {
        return Ok(());
    }

    let out = DockerManager::exec(&[
        "network",
        "create",
        "--internal",
        "--driver",
        "bridge",
        "--subnet",
        "172.30.0.0/16",
        name,
    ])?;

    if !out.success {
        bail!("network create failed: {}", out.stderr.trim());
    }

    tracing::info!("Created internal Docker network: {}", name);
    Ok(())
}

/// Create external network for egress proxy.
pub fn ensure_external_network(name: &str) -> Result<()> {
    let out = DockerManager::exec(&["network", "inspect", name])?;
    if out.success {
        return Ok(());
    }

    let out = DockerManager::exec(&["network", "create", "--driver", "bridge", name])?;

    if !out.success {
        bail!("external network create failed: {}", out.stderr.trim());
    }

    tracing::info!("Created external Docker network: {}", name);
    Ok(())
}

/// Remove a Docker network.
pub fn remove_network(name: &str) -> Result<()> {
    let out = DockerManager::exec(&["network", "rm", name])?;
    if !out.success && !out.stderr.contains("not found") {
        bail!("network rm failed: {}", out.stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires Docker daemon
    fn test_ensure_network_idempotent() {
        let name = "zcplatform-test-net";
        // Create
        ensure_network(name).unwrap();
        // Create again (should be idempotent)
        ensure_network(name).unwrap();
        // Cleanup
        let _ = remove_network(name);
    }
}
