//! HTTP client for communicating with the ZeroClaw gateway.

use anyhow::{Context, Result};

pub struct GatewayClient {
    pub(crate) base_url: String,
    pub(crate) token: Option<String>,
    client: reqwest::Client,
}

impl GatewayClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.to_string(),
            token: token.map(String::from),
            client,
        }
    }

    pub(crate) fn auth_header(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {t}"))
    }

    pub async fn get_status(&self) -> Result<serde_json::Value> {
        let mut req = self.client.get(format!("{}/api/status", self.base_url));
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await.context("status request failed")?;
        Ok(resp.json().await?)
    }

    pub async fn get_health(&self) -> Result<bool> {
        match self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
        {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    pub async fn get_devices(&self) -> Result<serde_json::Value> {
        let mut req = self.client.get(format!("{}/api/devices", self.base_url));
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await.context("devices request failed")?;
        Ok(resp.json().await?)
    }

    pub async fn initiate_pairing(&self) -> Result<serde_json::Value> {
        let mut req = self
            .client
            .post(format!("{}/api/pairing/initiate", self.base_url));
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await.context("pairing request failed")?;
        Ok(resp.json().await?)
    }

    /// Check whether the gateway requires pairing.
    pub async fn requires_pairing(&self) -> Result<bool> {
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .context("health request failed")?;
        let body: serde_json::Value = resp.json().await?;
        Ok(body["require_pairing"].as_bool().unwrap_or(false))
    }

    /// Request a new pairing code from the gateway (localhost-only admin endpoint).
    pub async fn request_new_paircode(&self) -> Result<String> {
        let resp = self
            .client
            .post(format!("{}/admin/paircode/new", self.base_url))
            .send()
            .await
            .context("paircode request failed")?;
        let body: serde_json::Value = resp.json().await?;
        body["pairing_code"]
            .as_str()
            .map(String::from)
            .context("no pairing_code in response")
    }

    /// Exchange a pairing code for a bearer token.
    pub async fn pair_with_code(&self, code: &str) -> Result<String> {
        let resp = self
            .client
            .post(format!("{}/pair", self.base_url))
            .header("X-Pairing-Code", code)
            .send()
            .await
            .context("pair request failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("pair request returned {}", resp.status());
        }
        let body: serde_json::Value = resp.json().await?;
        body["token"]
            .as_str()
            .map(String::from)
            .context("no token in pair response")
    }

    /// Validate an existing token by calling a protected endpoint.
    pub async fn validate_token(&self) -> Result<bool> {
        let mut req = self.client.get(format!("{}/api/status", self.base_url));
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        match req.send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    /// Auto-pair with the gateway: request a new code and exchange it for a token.
    pub async fn auto_pair(&self) -> Result<String> {
        let code = self.request_new_paircode().await?;
        self.pair_with_code(&code).await
    }

    pub async fn send_webhook_message(&self, message: &str) -> Result<serde_json::Value> {
        let mut req = self
            .client
            .post(format!("{}/webhook", self.base_url))
            .json(&serde_json::json!({ "message": message }));
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await.context("webhook request failed")?;
        Ok(resp.json().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_creation_no_token() {
        let client = GatewayClient::new("http://127.0.0.1:42617", None);
        assert_eq!(client.base_url, "http://127.0.0.1:42617");
        assert!(client.token.is_none());
        assert!(client.auth_header().is_none());
    }

    #[test]
    fn client_creation_with_token() {
        let client = GatewayClient::new("http://localhost:8080", Some("test-token"));
        assert_eq!(client.base_url, "http://localhost:8080");
        assert_eq!(client.token.as_deref(), Some("test-token"));
        assert_eq!(client.auth_header().unwrap(), "Bearer test-token");
    }

    #[test]
    fn client_custom_url() {
        let client = GatewayClient::new("https://zeroclaw.example.com:9999", None);
        assert_eq!(client.base_url, "https://zeroclaw.example.com:9999");
    }

    #[test]
    fn auth_header_format() {
        let client = GatewayClient::new("http://localhost", Some("zc_abc123"));
        assert_eq!(client.auth_header().unwrap(), "Bearer zc_abc123");
    }

    #[tokio::test]
    async fn health_returns_false_for_unreachable_host() {
        // Connect to a port that should not be listening.
        let client = GatewayClient::new("http://127.0.0.1:1", None);
        let result = client.get_health().await.unwrap();
        assert!(!result, "health should be false for unreachable host");
    }

    #[tokio::test]
    async fn status_fails_for_unreachable_host() {
        let client = GatewayClient::new("http://127.0.0.1:1", None);
        let result = client.get_status().await;
        assert!(result.is_err(), "status should fail for unreachable host");
    }

    #[tokio::test]
    async fn devices_fails_for_unreachable_host() {
        let client = GatewayClient::new("http://127.0.0.1:1", None);
        let result = client.get_devices().await;
        assert!(result.is_err(), "devices should fail for unreachable host");
    }

    #[tokio::test]
    async fn pairing_fails_for_unreachable_host() {
        let client = GatewayClient::new("http://127.0.0.1:1", None);
        let result = client.initiate_pairing().await;
        assert!(result.is_err(), "pairing should fail for unreachable host");
    }

    #[tokio::test]
    async fn webhook_fails_for_unreachable_host() {
        let client = GatewayClient::new("http://127.0.0.1:1", None);
        let result = client.send_webhook_message("hello").await;
        assert!(result.is_err(), "webhook should fail for unreachable host");
    }
}
