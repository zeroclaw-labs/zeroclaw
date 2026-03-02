use super::{kill_shared, new_shared_process, SharedProcess, Tunnel, TunnelProcess};
use anyhow::{bail, Result};
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/// Pinggy Tunnel — uses SSH to expose a local port via pinggy.io.
///
/// No separate binary required — uses the system `ssh` command.
/// Free tier works without a token; Pro features require a token
/// from dashboard.pinggy.io.
pub struct PinggyTunnel {
    token: Option<String>,
    region: Option<String>,
    proc: SharedProcess,
}

impl PinggyTunnel {
    pub fn new(token: Option<String>, region: Option<String>) -> Self {
        Self {
            token,
            region,
            proc: new_shared_process(),
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for PinggyTunnel {
    fn name(&self) -> &str {
        "pinggy"
    }

    async fn start(&self, local_host: &str, local_port: u16) -> Result<String> {
        // Pro tokens use pro.pinggy.io; free tier uses free.pinggy.io.
        let base = match self.token.as_deref() {
            Some(t) if !t.is_empty() => "pro.pinggy.io",
            _ => "free.pinggy.io",
        };
        let server_host = match self.region.as_deref() {
            Some(r) if !r.is_empty() => format!("{r}.{base}"),
            _ => base.into(),
        };

        // Build the SSH user portion: TOKEN@ or empty for free tier
        let destination = match self.token.as_deref() {
            Some(t) if !t.is_empty() => format!("{t}@{server_host}"),
            _ => server_host,
        };

        // Use the caller-provided local_host for forwarding target.
        let forward_spec = format!("0:{local_host}:{local_port}");

        let mut child = Command::new("ssh")
            .args([
                "-T",
                "-p",
                "443",
                "-R",
                &forward_spec,
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "ServerAliveInterval=30",
                &destination,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Pinggy may print the tunnel URL to stdout or stderr depending on
        // SSH mode; read both streams concurrently to catch it either way.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture pinggy stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture pinggy stderr"))?;

        let mut stdout_lines = tokio::io::BufReader::new(stdout).lines();
        let mut stderr_lines = tokio::io::BufReader::new(stderr).lines();
        let mut public_url = String::new();

        // Track stream state independently so EOF on one stream does not
        // abort reading the other — the URL may arrive on either stream.
        let mut stdout_done = false;
        let mut stderr_done = false;

        // Wait up to 15s for the tunnel URL to appear on either stream
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline && !(stdout_done && stderr_done) {
            let line = tokio::time::timeout(tokio::time::Duration::from_secs(3), async {
                if stdout_done {
                    return stderr_lines.next_line().await;
                }
                if stderr_done {
                    return stdout_lines.next_line().await;
                }
                tokio::select! {
                    l = stdout_lines.next_line() => l,
                    l = stderr_lines.next_line() => l,
                }
            })
            .await;

            match line {
                Ok(Ok(Some(l))) => {
                    tracing::debug!("pinggy: {l}");
                    // Pinggy prints tunnel URLs like: https://xxxxx.a.free.pinggy.link
                    // Skip non-tunnel URLs (e.g. dashboard.pinggy.io promo links).
                    if let Some(idx) = l.find("https://") {
                        let url_part = &l[idx..];
                        let end = url_part
                            .find(|c: char| c.is_whitespace())
                            .unwrap_or(url_part.len());
                        let candidate = &url_part[..end];
                        if candidate.contains(".pinggy.link") {
                            public_url = candidate.to_string();
                            break;
                        }
                    }
                }
                Ok(Ok(None)) => {
                    // One stream closed — mark it done but keep reading the other.
                    // We cannot determine which stream returned None from the
                    // select!, so mark both as potentially done if both were open.
                    // In practice, once we get None the select! will no longer
                    // return for that branch, so subsequent iterations read the
                    // remaining stream directly.
                    if !stdout_done {
                        stdout_done = true;
                    } else {
                        stderr_done = true;
                    }
                }
                Ok(Err(e)) => bail!("Error reading pinggy output: {e}"),
                Err(_) => {} // timeout — retry
            }
        }

        if public_url.is_empty() {
            child.kill().await.ok();
            child.wait().await.ok();
            bail!("pinggy did not produce a public URL within 15s. Is SSH available and the token valid?");
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
        let mut guard = self.proc.lock().await;
        match guard.as_mut() {
            Some(tp) => match tp.child.try_wait() {
                Ok(None) => true,  // still running
                Ok(Some(_)) => false, // exited
                Err(_) => false,
            },
            None => false,
        }
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
    fn name_returns_pinggy() {
        let tunnel = PinggyTunnel::new(None, None);
        assert_eq!(tunnel.name(), "pinggy");
    }

    #[test]
    fn constructor_stores_fields() {
        let tunnel = PinggyTunnel::new(Some("test-token".into()), Some("us".into()));
        assert_eq!(tunnel.token.as_deref(), Some("test-token"));
        assert_eq!(tunnel.region.as_deref(), Some("us"));
    }

    #[test]
    fn public_url_is_none_before_start() {
        let tunnel = PinggyTunnel::new(None, None);
        assert!(tunnel.public_url().is_none());
    }

    #[tokio::test]
    async fn stop_before_start_is_ok() {
        let tunnel = PinggyTunnel::new(None, None);
        let result = tunnel.stop().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn health_check_is_false_before_start() {
        let tunnel = PinggyTunnel::new(None, None);
        assert!(!tunnel.health_check().await);
    }
}
