//! HTTP client for communicating with the ZeroClaw gateway.
//!
//! A credential never goes to the dashboard address on trust alone when the
//! app launched the core: with a [`CoreLink`], every request that carries
//! the bearer token or a pairing code is sent on a connection that has just
//! proved it is that core's own gateway (see [`crate::possession`]). A
//! request that carries no credential, or a client without a core (a
//! gateway that was already running, or an older kernel), uses a plain
//! connection.

use crate::possession::CoreLink;
use anyhow::{Context, Result};
use std::sync::Arc;

pub struct GatewayClient {
    pub(crate) base_url: String,
    pub(crate) token: Option<String>,
    client: reqwest::Client,
    core: Option<Arc<CoreLink>>,
}

impl GatewayClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.to_string(),
            token: token.map(String::from),
            client,
            core: None,
        }
    }

    /// Send credentials only on connections proven to be `core`'s gateway.
    #[must_use]
    pub fn with_core(mut self, core: Option<Arc<CoreLink>>) -> Self {
        self.core = core;
        self
    }

    pub(crate) fn auth_header(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("Bearer {t}"))
    }

    /// One request that carries a credential (`Authorization` and, for
    /// pairing, the code): the status and body. With a core, it goes on a
    /// connection that proved itself just now; a failed proof sends nothing.
    async fn send_credential(
        &self,
        method: reqwest::Method,
        path: &str,
        pairing_code: Option<&str>,
        json: Option<serde_json::Value>,
    ) -> Result<(u16, Vec<u8>)> {
        let auth = self.auth_header();
        if let Some(core) = &self.core {
            let mut proven = core.prove().await.map_err(|failure| {
                anyhow::Error::msg(format!("refusing to send a credential: {failure}"))
            })?;
            let mut headers = Vec::new();
            if let Some(auth) = auth.as_deref() {
                headers.push(("Authorization", auth));
            }
            if let Some(code) = pairing_code {
                headers.push(("X-Pairing-Code", code));
            }
            let method =
                hyper::Method::from_bytes(method.as_str().as_bytes()).context("request method")?;
            let body = json.map(|json| json.to_string().into_bytes());
            let (status, body) = proven
                .send(method, path, &headers, body)
                .await
                .map_err(|error| anyhow::Error::msg(format!("{path} request failed: {error}")))?;
            return Ok((status, body.to_vec()));
        }
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.base_url));
        if let Some(auth) = auth {
            req = req.header("Authorization", auth);
        }
        if let Some(code) = pairing_code {
            req = req.header("X-Pairing-Code", code);
        }
        if let Some(json) = json {
            req = req.json(&json);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{path} request failed"))?;
        let status = resp.status().as_u16();
        Ok((status, resp.bytes().await?.to_vec()))
    }

    fn json_of(body: &[u8]) -> Result<serde_json::Value> {
        serde_json::from_slice(body).context("the gateway did not answer JSON")
    }

    pub async fn get_status(&self) -> Result<serde_json::Value> {
        let (_, body) = self
            .send_credential(reqwest::Method::GET, "/api/status", None, None)
            .await?;
        Self::json_of(&body)
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

    /// The `/health` body when the address answers with a success status:
    /// its JSON, or `Null` when the body is not JSON. `None` when nothing
    /// answers successfully.
    pub async fn health_report(&self) -> Option<serde_json::Value> {
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        Some(resp.json().await.unwrap_or(serde_json::Value::Null))
    }

    pub async fn get_devices(&self) -> Result<serde_json::Value> {
        let (_, body) = self
            .send_credential(reqwest::Method::GET, "/api/devices", None, None)
            .await?;
        Self::json_of(&body)
    }

    pub async fn initiate_pairing(&self) -> Result<serde_json::Value> {
        let (_, body) = self
            .send_credential(reqwest::Method::POST, "/api/pairing/initiate", None, None)
            .await?;
        Self::json_of(&body)
    }

    /// Whether the gateway requires pairing. With a core, the answer is the
    /// proven listener's own: the `/health` its proof was answered with. A
    /// failed proof, or an answer that does not say, is an error, never "no
    /// pairing". Without a core, the plain `/health` answer, `false` when it
    /// does not say.
    pub async fn requires_pairing(&self) -> Result<bool> {
        if let Some(core) = &self.core {
            let proven = core
                .prove()
                .await
                .map_err(|failure| anyhow::Error::msg(failure.to_string()))?;
            return proven.health()["require_pairing"]
                .as_bool()
                .context("its answer did not say whether it requires pairing");
        }
        let resp = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .context("health request failed")?;
        let body: serde_json::Value = resp.json().await?;
        Ok(body["require_pairing"].as_bool().unwrap_or(false))
    }

    /// Exchange a pairing code for a bearer token.
    pub async fn pair_with_code(&self, code: &str) -> Result<String> {
        let (status, body) = self
            .send_credential(reqwest::Method::POST, "/pair", Some(code), None)
            .await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("pair request returned {status}");
        }
        let body = Self::json_of(&body)?;
        body["token"]
            .as_str()
            .map(String::from)
            .context("no token in pair response")
    }

    /// Validate an existing token by calling a protected endpoint.
    pub async fn validate_token(&self) -> Result<bool> {
        match self
            .send_credential(reqwest::Method::GET, "/api/status", None, None)
            .await
        {
            Ok((status, _)) => Ok((200..300).contains(&status)),
            Err(_) => Ok(false),
        }
    }

    /// Push the device's currently-granted capabilities to the gateway so the agent
    /// knows what this Mac can do without waiting for a /ws/nodes connection.
    /// Requires a valid bearer token; the gateway uses its hash to identify the row.
    pub async fn update_capabilities(&self, capabilities: &[String]) -> Result<()> {
        let (status, _) = self
            .send_credential(
                reqwest::Method::POST,
                "/api/devices/me/capabilities",
                None,
                Some(serde_json::json!({ "capabilities": capabilities })),
            )
            .await
            .context("update_capabilities request failed")?;
        if !(200..300).contains(&status) {
            anyhow::bail!("update_capabilities returned {status}");
        }
        Ok(())
    }

    pub async fn send_webhook_message(&self, message: &str) -> Result<serde_json::Value> {
        let (_, body) = self
            .send_credential(
                reqwest::Method::POST,
                "/webhook",
                None,
                Some(serde_json::json!({ "message": message })),
            )
            .await
            .context("webhook request failed")?;
        Self::json_of(&body)
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
