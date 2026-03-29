use super::{SharedProcess, Tunnel, TunnelProcess, kill_shared, new_shared_process};
use anyhow::{Result, bail};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/// Try to extract a real tunnel URL from a cloudflared log line.
///
/// Returns `Some(url)` when the line contains a genuine tunnel endpoint,
/// skipping documentation and warning URLs (quic-go GitHub links,
/// Cloudflare docs pages, etc.).
fn extract_tunnel_url(line: &str) -> Option<String> {
    let idx = line.find("https://")?;
    let url_part = &line[idx..];
    let end = url_part
        .find(|c: char| c.is_whitespace())
        .unwrap_or(url_part.len());
    let candidate = &url_part[..end];

    let is_tunnel_line = line.contains("Visit it at")
        || line.contains("Route at")
        || line.contains("Registered tunnel connection");
    let is_tunnel_domain = candidate.contains(".trycloudflare.com");
    let is_docs_url = candidate.contains("github.com")
        || candidate.contains("cloudflare.com/docs")
        || candidate.contains("developers.cloudflare.com");

    if is_tunnel_line || is_tunnel_domain || !is_docs_url {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Cloudflare Tunnel — wraps the `cloudflared` binary.
///
/// Requires `cloudflared` installed and a tunnel token from the
/// Cloudflare Zero Trust dashboard.
pub struct CloudflareTunnel {
    token: String,
    proc: SharedProcess,
}

impl CloudflareTunnel {
    pub fn new(token: String) -> Self {
        Self {
            token,
            proc: new_shared_process(),
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for CloudflareTunnel {
    fn name(&self) -> &str {
        "cloudflare"
    }

    async fn start(&self, _local_host: &str, local_port: u16) -> Result<String> {
        // cloudflared tunnel --no-autoupdate run --url http://localhost:<port>
        // Token passed via env var to avoid exposure in ps/proc on shared systems.
        let mut child = Command::new("cloudflared")
            .args([
                "tunnel",
                "--no-autoupdate",
                "run",
                "--url",
                &format!("http://localhost:{local_port}"),
            ])
            .env("TUNNEL_TOKEN", &self.token)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Read stderr to find the public URL (cloudflared prints it there)
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture cloudflared stderr"))?;

        let mut reader = tokio::io::BufReader::new(stderr).lines();
        let mut public_url = String::new();

        // Wait up to 30s for the tunnel URL to appear
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            let line =
                tokio::time::timeout(tokio::time::Duration::from_secs(5), reader.next_line()).await;

            match line {
                Ok(Ok(Some(l))) => {
                    tracing::debug!("cloudflared: {l}");
                    if let Some(url) = extract_tunnel_url(&l) {
                        public_url = url;
                        break;
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => bail!("Error reading cloudflared output: {e}"),
                Err(_) => {} // timeout on this line, keep trying
            }
        }

        if public_url.is_empty() {
            child.kill().await.ok();
            bail!("cloudflared did not produce a public URL within 30s. Is the token valid?");
        }

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    async fn stop(&self) -> Result<()> {
        kill_shared(&self.proc).await
    }

    async fn health_check(&self) -> bool {
        let guard = self.proc.lock().await;
        guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
    }

    fn public_url(&self) -> Option<String> {
        // Can't block on async lock in a sync fn, so we try_lock
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
    fn constructor_stores_token() {
        let tunnel = CloudflareTunnel::new("cf-token".into());
        assert_eq!(tunnel.token, "cf-token");
    }

    #[test]
    fn public_url_is_none_before_start() {
        let tunnel = CloudflareTunnel::new("cf-token".into());
        assert!(tunnel.public_url().is_none());
    }

    #[tokio::test]
    async fn stop_without_started_process_is_ok() {
        let tunnel = CloudflareTunnel::new("cf-token".into());
        let result = tunnel.stop().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn health_check_is_false_before_start() {
        let tunnel = CloudflareTunnel::new("cf-token".into());
        assert!(!tunnel.health_check().await);
    }

    #[test]
    fn extract_skips_quic_go_github_url() {
        let line = "2024-01-01T00:00:00Z WRN failed to sufficiently increase receive buffer size. See https://github.com/quic-go/quic-go/wiki/UDP-Buffer-Sizes for details.";
        assert_eq!(extract_tunnel_url(line), None);
    }

    #[test]
    fn extract_skips_cloudflare_docs_url() {
        let line = "2024-01-01T00:00:00Z INF For more info see https://cloudflare.com/docs/tunnels";
        assert_eq!(extract_tunnel_url(line), None);
    }

    #[test]
    fn extract_skips_developers_cloudflare_url() {
        let line = "2024-01-01T00:00:00Z INF See https://developers.cloudflare.com/cloudflare-one/connections/connect-apps";
        assert_eq!(extract_tunnel_url(line), None);
    }

    #[test]
    fn extract_captures_trycloudflare_url() {
        let line = "2024-01-01T00:00:00Z INF Visit it at https://my-tunnel-abc.trycloudflare.com";
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://my-tunnel-abc.trycloudflare.com".into())
        );
    }

    #[test]
    fn extract_captures_url_on_visit_it_at_line() {
        let line = "2024-01-01T00:00:00Z INF Visit it at https://some-custom-domain.example.com";
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://some-custom-domain.example.com".into())
        );
    }

    #[test]
    fn extract_captures_url_on_route_at_line() {
        let line = "2024-01-01T00:00:00Z INF Route at https://tunnel.example.com/path";
        assert_eq!(
            extract_tunnel_url(line),
            Some("https://tunnel.example.com/path".into())
        );
    }

    #[test]
    fn extract_returns_none_for_line_without_url() {
        let line = "2024-01-01T00:00:00Z INF Starting tunnel";
        assert_eq!(extract_tunnel_url(line), None);
    }
}
