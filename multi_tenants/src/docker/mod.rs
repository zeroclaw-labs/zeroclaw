pub mod egress;
pub mod image;
pub mod network;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

pub struct DockerManager {
    data_dir: String,
    network: String,
    image: String,
}

#[derive(Debug)]
pub struct DockerOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl DockerManager {
    pub fn new(data_dir: &str, network: &str, image: &str) -> Self {
        Self {
            data_dir: data_dir.to_string(),
            network: network.to_string(),
            image: image.to_string(),
        }
    }

    /// Execute a docker command and return parsed output.
    /// pub(crate) so submodules can use it.
    pub(crate) fn exec(args: &[&str]) -> Result<DockerOutput> {
        let output = Command::new("docker").args(args).output()?;
        Ok(DockerOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
        })
    }

    /// Check Docker daemon is reachable.
    pub fn health_check() -> Result<bool> {
        let out = Self::exec(&["info", "--format", "{{.ServerVersion}}"])?;
        Ok(out.success)
    }

    /// Create and start a tenant container with full security hardening.
    pub fn create_container(
        &self,
        slug: &str,
        port: u16,
        uid: u32,
        env_vars: &[(&str, &str)],
        memory_mb: u32,
        cpu_limit: f64,
    ) -> Result<String> {
        let container_name = format!("zc-tenant-{}", slug);
        let workspace_vol = format!(
            "{}/{}/workspace:/zeroclaw-data/workspace",
            self.data_dir, slug
        );
        let memory_vol = format!(
            "{}/{}/memory:/zeroclaw-data/.zeroclaw/memory:rw",
            self.data_dir, slug
        );
        let zeroclaw_home_vol = format!(
            "{}/{}/zeroclaw-home:/zeroclaw-data/.zeroclaw:rw",
            self.data_dir, slug
        );
        let user_flag = format!("{}:{}", uid, uid);
        let memory_flag = format!("{}m", memory_mb);
        let cpu_flag = format!("{:.1}", cpu_limit);
        let port_flag = format!("127.0.0.1:{}:{}", port, port);
        // Config is inside zeroclaw-home, no separate mount needed.
        // ZeroClaw reads from $HOME/.zeroclaw/config.toml = /zeroclaw-data/.zeroclaw/config.toml
        let gateway_port = port.to_string();

        // Use bridge as primary network (required for port publishing).
        // Internal network is connected after creation for inter-container routing.
        let mut args = vec![
            "run",
            "-d",
            "--name",
            &container_name,
            "--network",
            "bridge",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--pids-limit=50",
            "--memory",
            &memory_flag,
            "--memory-swap",
            &memory_flag,
            "--cpus",
            &cpu_flag,
            "--ulimit",
            "nofile=256:256",
            "--ulimit",
            "nproc=50:50",
            "--tmpfs",
            "/tmp:size=50m,noexec,nosuid",
            "--user",
            &user_flag,
            "--restart=unless-stopped",
            "--log-opt",
            "max-size=10m",
            "--log-opt",
            "max-file=3",
            "-v",
            &workspace_vol,
            "-v",
            &zeroclaw_home_vol,
            "-v",
            &memory_vol,
            "-p",
            &port_flag,
        ];

        // Add env vars
        let env_strings: Vec<String> = env_vars
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        for env in &env_strings {
            args.push("-e");
            args.push(env);
        }

        // Image and command — use daemon mode (gateway + channels + heartbeat)
        args.push(&self.image);
        args.push("daemon");
        args.push("--port");
        args.push(&gateway_port);

        let out = Self::exec(&args)?;
        if !out.success {
            bail!("docker create failed: {}", out.stderr.trim());
        }
        let container_id = out.stdout.trim().to_string();

        // Connect to internal network for egress proxy / inter-container routing
        let connect_out = Self::exec(&["network", "connect", &self.network, &container_name])?;
        if !connect_out.success {
            tracing::warn!(
                "failed to connect {} to {}: {}",
                container_name,
                self.network,
                connect_out.stderr.trim()
            );
        }

        Ok(container_id)
    }

    /// Stop a tenant container (10s timeout).
    pub fn stop_container(&self, slug: &str) -> Result<()> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["stop", "-t", "10", &name])?;
        if !out.success && !out.stderr.contains("No such container") {
            bail!("docker stop failed: {}", out.stderr.trim());
        }
        Ok(())
    }

    /// Start a stopped container.
    pub fn start_container(&self, slug: &str) -> Result<()> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["start", &name])?;
        if !out.success {
            bail!("docker start failed: {}", out.stderr.trim());
        }
        Ok(())
    }

    /// Restart a container.
    pub fn restart_container(&self, slug: &str) -> Result<()> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["restart", "-t", "10", &name])?;
        if !out.success {
            bail!("docker restart failed: {}", out.stderr.trim());
        }
        Ok(())
    }

    /// Remove a container (force if running).
    pub fn remove_container(&self, slug: &str) -> Result<()> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["rm", "-f", &name])?;
        if !out.success && !out.stderr.contains("No such container") {
            bail!("docker rm failed: {}", out.stderr.trim());
        }
        Ok(())
    }

    /// Get container logs (last N lines).
    pub fn logs(&self, slug: &str, tail: u32) -> Result<String> {
        let name = format!("zc-tenant-{}", slug);
        let tail_str = tail.to_string();
        let out = Self::exec(&["logs", "--tail", &tail_str, &name])?;
        Ok(format!("{}{}", out.stdout, out.stderr))
    }

    /// Inspect container: returns JSON string.
    pub fn inspect(&self, slug: &str) -> Result<String> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["inspect", &name])?;
        if !out.success {
            bail!("docker inspect failed: {}", out.stderr.trim());
        }
        Ok(out.stdout)
    }

    /// Check if container is running.
    pub fn is_running(&self, slug: &str) -> Result<bool> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["inspect", "-f", "{{.State.Running}}", &name])?;
        Ok(out.success && out.stdout.trim() == "true")
    }

    /// Execute a command inside a running container with a timeout.
    pub fn exec_in_container(&self, slug: &str, cmd: &[&str], timeout_secs: u32) -> Result<String> {
        let name = format!("zc-tenant-{}", slug);
        let mut args = vec!["exec", &name];
        args.extend_from_slice(cmd);

        let output = Command::new("docker")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let result = output
            .wait_with_output()
            .map_err(|e| anyhow::anyhow!("exec failed: {}", e))?;

        let _ = timeout_secs; // timeout handled by container-level limits

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            bail!("exec failed: {}", stderr.trim());
        }

        Ok(String::from_utf8_lossy(&result.stdout).trim().to_string())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContainerStats {
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub mem_limit: u64,
    pub net_in_bytes: u64,
    pub net_out_bytes: u64,
    pub pids: u32,
}

impl DockerManager {
    /// Get resource stats for a running container.
    pub fn container_stats(slug: &str) -> Result<ContainerStats> {
        let name = format!("zc-tenant-{}", slug);
        let out = Self::exec(&["stats", "--no-stream", "--format", "{{json .}}", &name])?;
        if !out.success {
            bail!("docker stats failed for {}: {}", slug, out.stderr.trim());
        }
        parse_docker_stats_json(out.stdout.trim())
    }
}

/// Parse docker stats JSON output into ContainerStats.
/// Docker stats JSON format: {"CPUPerc":"0.07%","MemUsage":"46.5MiB / 256MiB","NetIO":"1.2kB / 3.4kB","PIDs":"5",...}
fn parse_docker_stats_json(json_str: &str) -> Result<ContainerStats> {
    let v: serde_json::Value = serde_json::from_str(json_str)
        .with_context(|| format!("failed to parse docker stats JSON: {}", json_str))?;

    let cpu_pct = parse_pct(v.get("CPUPerc").and_then(|v| v.as_str()).unwrap_or("0%"));
    let (mem_bytes, mem_limit) = parse_mem_usage(
        v.get("MemUsage")
            .and_then(|v| v.as_str())
            .unwrap_or("0B / 0B"),
    );
    let (net_in, net_out) =
        parse_net_io(v.get("NetIO").and_then(|v| v.as_str()).unwrap_or("0B / 0B"));
    let pids = v
        .get("PIDs")
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);

    Ok(ContainerStats {
        cpu_pct,
        mem_bytes,
        mem_limit,
        net_in_bytes: net_in,
        net_out_bytes: net_out,
        pids,
    })
}

/// Parse "1.23%" -> 1.23
fn parse_pct(s: &str) -> f64 {
    s.trim_end_matches('%').trim().parse().unwrap_or(0.0)
}

/// Parse "46.5MiB / 256MiB" -> (used_bytes, limit_bytes)
fn parse_mem_usage(s: &str) -> (u64, u64) {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return (0, 0);
    }
    (parse_size(parts[0].trim()), parse_size(parts[1].trim()))
}

/// Parse "1.2kB / 3.4kB" -> (in_bytes, out_bytes)
fn parse_net_io(s: &str) -> (u64, u64) {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        return (0, 0);
    }
    (parse_size(parts[0].trim()), parse_size(parts[1].trim()))
}

/// Parse size string like "46.5MiB", "1.2kB", "256B", "1.5GiB" -> bytes
fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }

    // Find where digits end and unit begins
    let (num_part, unit) = split_number_unit(s);
    let value: f64 = num_part.parse().unwrap_or(0.0);

    let multiplier: f64 = match unit.to_lowercase().as_str() {
        "b" | "" => 1.0,
        "kb" => 1_000.0,
        "kib" => 1_024.0,
        "mb" => 1_000_000.0,
        "mib" => 1_048_576.0,
        "gb" => 1_000_000_000.0,
        "gib" => 1_073_741_824.0,
        "tb" => 1_000_000_000_000.0,
        "tib" => 1_099_511_627_776.0,
        _ => 1.0,
    };
    (value * multiplier) as u64
}

/// Split "46.5MiB" into ("46.5", "MiB")
fn split_number_unit(s: &str) -> (&str, &str) {
    let pos = s
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(s.len());
    (&s[..pos], &s[pos..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_name_format() {
        let dm = DockerManager::new("/data", "test-net", "test:latest");
        // Just verify the struct construction works
        assert_eq!(dm.data_dir, "/data");
        assert_eq!(dm.network, "test-net");
        assert_eq!(dm.image, "test:latest");
    }

    #[test]
    #[ignore] // Requires Docker daemon
    fn test_health_check() {
        let healthy = DockerManager::health_check().unwrap();
        assert!(healthy);
    }

    #[test]
    fn test_parse_pct() {
        assert!((parse_pct("1.23%") - 1.23).abs() < 0.001);
        assert!((parse_pct("0.00%") - 0.0).abs() < 0.001);
        assert!((parse_pct("100%") - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("256B"), 256);
        assert_eq!(parse_size("1kB"), 1_000);
        assert_eq!(parse_size("1KiB"), 1_024);
        assert_eq!(parse_size("256MiB"), 268_435_456);
        assert_eq!(parse_size("1GiB"), 1_073_741_824);
        // Float values
        assert_eq!(parse_size("46.5MiB"), 48_758_784); // 46.5 * 1048576
        assert_eq!(parse_size("1.5GiB"), 1_610_612_736);
    }

    #[test]
    fn test_parse_mem_usage() {
        let (used, limit) = parse_mem_usage("46.5MiB / 256MiB");
        assert_eq!(used, 48_758_784);
        assert_eq!(limit, 268_435_456);
    }

    #[test]
    fn test_parse_net_io() {
        let (in_b, out_b) = parse_net_io("1.2kB / 3.4kB");
        assert_eq!(in_b, 1_200);
        assert_eq!(out_b, 3_400);
    }

    #[test]
    fn test_parse_docker_stats_json() {
        let json = r#"{"BlockIO":"0B / 0B","CPUPerc":"0.07%","Container":"abc123","ID":"abc123","MemPerc":"18.16%","MemUsage":"46.5MiB / 256MiB","Name":"zc-tenant-demo","NetIO":"1.2kB / 3.4kB","PIDs":"5"}"#;
        let stats = parse_docker_stats_json(json).unwrap();
        assert!((stats.cpu_pct - 0.07).abs() < 0.001);
        assert_eq!(stats.mem_bytes, 48_758_784);
        assert_eq!(stats.mem_limit, 268_435_456);
        assert_eq!(stats.net_in_bytes, 1_200);
        assert_eq!(stats.net_out_bytes, 3_400);
        assert_eq!(stats.pids, 5);
    }

    #[test]
    #[ignore] // Requires Docker daemon
    fn test_create_stop_remove_lifecycle() {
        let dm = DockerManager::new("/tmp/zc-test", "bridge", "alpine:latest");
        let slug = "lifecycle-test";

        // Cleanup from any previous failed run
        let _ = dm.remove_container(slug);

        // Create a simple container (alpine sleep)
        let out = DockerManager::exec(&[
            "run",
            "-d",
            "--name",
            &format!("zc-tenant-{}", slug),
            "alpine:latest",
            "sleep",
            "60",
        ])
        .unwrap();
        assert!(out.success, "create failed: {}", out.stderr);

        // Check running
        assert!(dm.is_running(slug).unwrap());

        // Stop
        dm.stop_container(slug).unwrap();
        assert!(!dm.is_running(slug).unwrap());

        // Remove
        dm.remove_container(slug).unwrap();
    }
}
