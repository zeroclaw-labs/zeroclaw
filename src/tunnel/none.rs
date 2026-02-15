use super::Tunnel;
use anyhow::Result;

/// No-op tunnel — direct local access, no external exposure.
pub struct NoneTunnel;

#[async_trait::async_trait]
impl Tunnel for NoneTunnel {
    fn name(&self) -> &str {
        "none"
    }

    async fn start(&self, local_host: &str, local_port: u16) -> Result<String> {
        Ok(format!("http://{local_host}:{local_port}"))
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }

    fn public_url(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_tunnel_name_returns_none() {
        let tunnel = NoneTunnel;
        assert_eq!(tunnel.name(), "none");
    }

    #[test]
    fn none_tunnel_public_url_always_none() {
        let tunnel = NoneTunnel;
        assert!(tunnel.public_url().is_none());
    }

    #[tokio::test]
    async fn none_tunnel_start_returns_local_url() {
        let tunnel = NoneTunnel;
        let url = tunnel.start("127.0.0.1", 8080).await.unwrap();
        assert_eq!(url, "http://127.0.0.1:8080");
    }

    #[tokio::test]
    async fn none_tunnel_start_with_custom_host_port() {
        let tunnel = NoneTunnel;
        let url = tunnel.start("localhost", 3000).await.unwrap();
        assert_eq!(url, "http://localhost:3000");
    }

    #[tokio::test]
    async fn none_tunnel_health_check_always_true() {
        let tunnel = NoneTunnel;
        assert!(tunnel.health_check().await);
    }

    #[tokio::test]
    async fn none_tunnel_stop_always_succeeds() {
        let tunnel = NoneTunnel;
        let result = tunnel.stop().await;
        assert!(result.is_ok());
    }
}
