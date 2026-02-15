use super::{kill_shared, new_shared_process, SharedProcess, Tunnel, TunnelProcess};
use anyhow::{bail, Result};
use tokio::process::Command;

/// Tailscale Tunnel — uses `tailscale serve` (tailnet-only) or
/// `tailscale funnel` (public internet).
///
/// Requires Tailscale installed and authenticated (`tailscale up`).
pub struct TailscaleTunnel {
    funnel: bool,
    hostname: Option<String>,
    proc: SharedProcess,
}

impl TailscaleTunnel {
    pub fn new(funnel: bool, hostname: Option<String>) -> Self {
        Self {
            funnel,
            hostname,
            proc: new_shared_process(),
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for TailscaleTunnel {
    fn name(&self) -> &str {
        "tailscale"
    }

    async fn start(&self, _local_host: &str, local_port: u16) -> Result<String> {
        let subcommand = if self.funnel { "funnel" } else { "serve" };

        // Get the tailscale hostname for URL construction
        let hostname = if let Some(ref h) = self.hostname {
            h.clone()
        } else {
            // Query tailscale for the current hostname
            let output = Command::new("tailscale")
                .args(["status", "--json"])
                .output()
                .await?;

            if !output.status.success() {
                bail!(
                    "tailscale status failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            let status: serde_json::Value =
                serde_json::from_slice(&output.stdout).unwrap_or_default();
            status["Self"]["DNSName"]
                .as_str()
                .unwrap_or("localhost")
                .trim_end_matches('.')
                .to_string()
        };

        // tailscale serve|funnel <port>
        let child = Command::new("tailscale")
            .args([subcommand, &local_port.to_string()])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let public_url = format!("https://{hostname}:{local_port}");

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    async fn stop(&self) -> Result<()> {
        // Also reset the tailscale serve/funnel
        let subcommand = if self.funnel { "funnel" } else { "serve" };
        Command::new("tailscale")
            .args([subcommand, "reset"])
            .output()
            .await
            .ok();

        kill_shared(&self.proc).await
    }

    async fn health_check(&self) -> bool {
        let guard = self.proc.lock().await;
        guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
    }

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
    fn tailscale_new_creates_valid_instance() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert!(!tunnel.funnel);
        assert!(tunnel.hostname.is_none());
        assert_eq!(tunnel.name(), "tailscale");
    }

    #[test]
    fn tailscale_new_with_funnel_and_hostname() {
        let tunnel = TailscaleTunnel::new(true, Some("myhost.tail-scale.ts.net".into()));
        assert!(tunnel.funnel);
        assert_eq!(tunnel.hostname, Some("myhost.tail-scale.ts.net".into()));
        assert_eq!(tunnel.name(), "tailscale");
    }

    #[test]
    fn tailscale_name_returns_tailscale() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert_eq!(tunnel.name(), "tailscale");
    }

    #[test]
    fn tailscale_public_url_none_before_start() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert!(tunnel.public_url().is_none());
    }

    #[tokio::test]
    async fn tailscale_start_returns_result() {
        let tunnel = TailscaleTunnel::new(false, None);
        let _result = tunnel.start("127.0.0.1", 8080).await;
    }

    #[tokio::test]
    async fn tailscale_health_check_false_before_start() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn tailscale_stop_succeeds_when_not_started() {
        let tunnel = TailscaleTunnel::new(false, None);
        let result = tunnel.stop().await;
        assert!(result.is_ok());
    }
}
