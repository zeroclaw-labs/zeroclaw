use super::{kill_shared, new_shared_process, SharedProcess, Tunnel, TunnelProcess};
use anyhow::{bail, Result};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/// OpenVPN Tunnel — uses the `openvpn` CLI to establish a VPN connection.
///
/// Requires the `openvpn` binary installed and accessible. On most systems,
/// OpenVPN requires root/administrator privileges to create tun/tap devices.
///
/// The tunnel exposes the gateway via the VPN network using a configured
/// `advertise_address` (e.g., `"10.8.0.2:42617"`).
pub struct OpenVpnTunnel {
    config_file: String,
    auth_file: Option<String>,
    advertise_address: Option<String>,
    connect_timeout_secs: u64,
    extra_args: Vec<String>,
    proc: SharedProcess,
}

impl OpenVpnTunnel {
    /// Create a new OpenVPN tunnel instance.
    ///
    /// * `config_file` — path to the `.ovpn` configuration file.
    /// * `auth_file` — optional path to a credentials file for `--auth-user-pass`.
    /// * `advertise_address` — optional public address to advertise once connected.
    /// * `connect_timeout_secs` — seconds to wait for the initialization sequence.
    /// * `extra_args` — additional CLI arguments forwarded to the `openvpn` binary.
    pub fn new(
        config_file: String,
        auth_file: Option<String>,
        advertise_address: Option<String>,
        connect_timeout_secs: u64,
        extra_args: Vec<String>,
    ) -> Self {
        Self {
            config_file,
            auth_file,
            advertise_address,
            connect_timeout_secs,
            extra_args,
            proc: new_shared_process(),
        }
    }

    /// Build the openvpn command arguments.
    fn build_args(&self) -> Vec<String> {
        let mut args = vec!["--config".to_string(), self.config_file.clone()];

        if let Some(ref auth) = self.auth_file {
            args.push("--auth-user-pass".to_string());
            args.push(auth.clone());
        }

        args.extend(self.extra_args.iter().cloned());
        args
    }
}

#[async_trait::async_trait]
impl Tunnel for OpenVpnTunnel {
    fn name(&self) -> &str {
        "openvpn"
    }

    /// Spawn the `openvpn` process and wait for the "Initialization Sequence
    /// Completed" marker on stderr. Returns the public URL on success.
    async fn start(&self, local_host: &str, local_port: u16) -> Result<String> {
        // Validate config file exists before spawning
        if !std::path::Path::new(&self.config_file).exists() {
            bail!("OpenVPN config file not found: {}", self.config_file);
        }

        let args = self.build_args();

        let mut child = Command::new("openvpn")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Wait for "Initialization Sequence Completed" in stderr
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture openvpn stderr"))?;

        let mut reader = tokio::io::BufReader::new(stderr).lines();
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_secs(self.connect_timeout_secs);

        let mut connected = false;
        while tokio::time::Instant::now() < deadline {
            let line =
                tokio::time::timeout(tokio::time::Duration::from_secs(3), reader.next_line()).await;

            match line {
                Ok(Ok(Some(l))) => {
                    tracing::debug!("openvpn: {l}");
                    if l.contains("Initialization Sequence Completed") {
                        connected = true;
                        break;
                    }
                }
                Ok(Ok(None)) => {
                    bail!("OpenVPN process exited before connection was established");
                }
                Ok(Err(e)) => {
                    bail!("Error reading openvpn output: {e}");
                }
                Err(_) => {
                    // Timeout on individual line read, continue waiting
                }
            }
        }

        if !connected {
            child.kill().await.ok();
            bail!(
                "OpenVPN connection timed out after {}s waiting for initialization",
                self.connect_timeout_secs
            );
        }

        let public_url = self
            .advertise_address
            .clone()
            .unwrap_or_else(|| format!("http://{local_host}:{local_port}"));

        // Drain stderr in background to prevent OS pipe buffer from filling and
        // blocking the openvpn process.
        tokio::spawn(async move {
            while let Ok(Some(line)) = reader.next_line().await {
                tracing::trace!("openvpn: {line}");
            }
        });

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    /// Kill the openvpn child process and release its resources.
    async fn stop(&self) -> Result<()> {
        kill_shared(&self.proc).await
    }

    /// Return `true` if the openvpn child process is still running.
    async fn health_check(&self) -> bool {
        let guard = self.proc.lock().await;
        guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
    }

    /// Return the public URL if the tunnel has been started.
    fn public_url(&self) -> Option<String> {
        self.proc
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tp| tp.public_url.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_stores_fields() {
        let tunnel = OpenVpnTunnel::new(
            "/etc/openvpn/client.ovpn".into(),
            Some("/etc/openvpn/auth.txt".into()),
            Some("10.8.0.2:42617".into()),
            45,
            vec!["--verb".into(), "3".into()],
        );
        assert_eq!(tunnel.config_file, "/etc/openvpn/client.ovpn");
        assert_eq!(tunnel.auth_file.as_deref(), Some("/etc/openvpn/auth.txt"));
        assert_eq!(tunnel.advertise_address.as_deref(), Some("10.8.0.2:42617"));
        assert_eq!(tunnel.connect_timeout_secs, 45);
        assert_eq!(tunnel.extra_args, vec!["--verb", "3"]);
    }

    #[test]
    fn build_args_basic() {
        let tunnel = OpenVpnTunnel::new("client.ovpn".into(), None, None, 30, vec![]);
        let args = tunnel.build_args();
        assert_eq!(args, vec!["--config", "client.ovpn"]);
    }

    #[test]
    fn build_args_with_auth_and_extras() {
        let tunnel = OpenVpnTunnel::new(
            "client.ovpn".into(),
            Some("auth.txt".into()),
            None,
            30,
            vec!["--verb".into(), "5".into()],
        );
        let args = tunnel.build_args();
        assert_eq!(
            args,
            vec![
                "--config",
                "client.ovpn",
                "--auth-user-pass",
                "auth.txt",
                "--verb",
                "5"
            ]
        );
    }

    #[test]
    fn public_url_is_none_before_start() {
        let tunnel = OpenVpnTunnel::new("client.ovpn".into(), None, None, 30, vec![]);
        assert!(tunnel.public_url().is_none());
    }

    #[tokio::test]
    async fn health_check_is_false_before_start() {
        let tunnel = OpenVpnTunnel::new("client.ovpn".into(), None, None, 30, vec![]);
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn stop_without_started_process_is_ok() {
        let tunnel = OpenVpnTunnel::new("client.ovpn".into(), None, None, 30, vec![]);
        let result = tunnel.stop().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn start_with_missing_config_file_errors() {
        let tunnel = OpenVpnTunnel::new(
            "/nonexistent/path/to/client.ovpn".into(),
            None,
            None,
            30,
            vec![],
        );
        let result = tunnel.start("127.0.0.1", 8080).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("config file not found"));
    }
}
