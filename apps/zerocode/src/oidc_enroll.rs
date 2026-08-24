//! Device-grant enrollment against the daemon gateway's OIDC API.
//!
//! zerocode holds no IdP client credentials and, by gate, no zeroclaw
//! crate dependencies, so it enrolls through the gateway's cross-surface
//! enrollment API (`/api/oidc/{alias}/device/*`), which proxies the
//! RFC 8628 device grant with the `[oidc.<alias>]` entry's credentials.
//! The resulting access token is held in memory for this session only
//! and presented as `auth_token` in the RPC handshake; nothing is
//! stored.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_poll_interval")]
    pub interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct PollResponse {
    status: String,
    #[serde(default)]
    token: Option<PollToken>,
}

#[derive(Debug, Deserialize)]
struct PollToken {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct GatewayError {
    error: String,
}

pub(crate) struct GatewayEnrollment {
    base: String,
    http: reqwest::Client,
}

impl GatewayEnrollment {
    /// `base` is the gateway's HTTP origin (`[connection.wss] enroll_url`).
    /// `skip_verify` mirrors the WSS TLS setting so a self-signed gateway
    /// works the same way for both connections.
    pub fn new(base: &str, skip_verify: bool) -> Result<Self> {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
        if skip_verify {
            builder = builder.danger_accept_invalid_certs(true);
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http: builder.build()?,
        })
    }

    async fn gateway_error(response: reqwest::Response) -> anyhow::Error {
        let status = response.status();
        match response.json::<GatewayError>().await {
            Ok(err) => anyhow::Error::msg(err.error),
            Err(_) => anyhow::Error::msg(format!("gateway returned HTTP {status}")),
        }
    }

    pub async fn device_start(&self, alias: &str) -> Result<DeviceStart> {
        let response = self
            .http
            .post(format!("{}/api/oidc/{alias}/device/start", self.base))
            .send()
            .await
            .context("cannot reach the gateway enrollment API")?;
        if !response.status().is_success() {
            return Err(Self::gateway_error(response).await);
        }
        response
            .json()
            .await
            .context("device start response is not valid JSON")
    }

    /// One poll. `Ok(Some(token))` on grant, `Ok(None)` while pending
    /// (`slow_down` is folded in by growing the caller's interval).
    pub async fn device_poll(&self, alias: &str, device_code: &str) -> Result<Option<String>> {
        let response = self
            .http
            .post(format!("{}/api/oidc/{alias}/device/poll", self.base))
            .json(&serde_json::json!({ "device_code": device_code }))
            .send()
            .await
            .context("cannot reach the gateway enrollment API")?;
        if !response.status().is_success() {
            return Err(Self::gateway_error(response).await);
        }
        let poll: PollResponse = response
            .json()
            .await
            .context("device poll response is not valid JSON")?;
        match poll.status.as_str() {
            "granted" => match poll.token {
                Some(token) => Ok(Some(token.access_token)),
                None => bail!("gateway reported a grant without a token"),
            },
            "pending" | "slow_down" => Ok(None),
            other => bail!("unexpected poll status from the gateway: {other}"),
        }
    }
}

/// Run the whole device flow: start, hand the prompt data to `on_prompt`
/// (rendered by the caller so wording stays with the i18n layer), poll
/// until granted, denied, or the code expires.
pub(crate) async fn run_device_flow(
    base: &str,
    skip_verify: bool,
    alias: &str,
    on_prompt: impl Fn(&DeviceStart),
) -> Result<String> {
    let gateway = GatewayEnrollment::new(base, skip_verify)?;
    let start = gateway.device_start(alias).await?;
    on_prompt(&start);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(start.expires_in);
    let mut interval = start.interval.max(1);
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("the device code expired before approval");
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match gateway.device_poll(alias, &start.device_code).await? {
            Some(token) => return Ok(token),
            // The gateway folds the IdP's slow_down into "pending" for
            // clients that poll on the advertised interval; growing by a
            // second per round keeps a long wait polite either way.
            None => interval = interval.saturating_add(1).min(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn device_start_parses_the_gateway_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-1",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), false).unwrap();
        let start = gateway.device_start("corp").await.unwrap();
        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(start.device_code, "dev-1");
    }

    #[tokio::test]
    async fn device_poll_maps_pending_and_granted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("waiting"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "status": "pending" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("done"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "granted",
                "provider": "oidc.corp",
                "token": { "access_token": "at-tui", "expires_in": 3600 },
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), false).unwrap();
        assert_eq!(gateway.device_poll("corp", "waiting").await.unwrap(), None);
        assert_eq!(
            gateway
                .device_poll("corp", "done")
                .await
                .unwrap()
                .as_deref(),
            Some("at-tui")
        );
    }

    #[tokio::test]
    async fn gateway_denials_surface_the_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .respond_with(ResponseTemplate::new(502).set_body_json(serde_json::json!({
                "error": "device grant failed: access_denied (user rejected the request)",
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), false).unwrap();
        let err = gateway.device_poll("corp", "dev-1").await.unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");
    }

    #[tokio::test]
    async fn unknown_alias_is_surfaced() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/nope/device/start"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": "unknown oidc provider alias",
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), false).unwrap();
        let err = gateway.device_start("nope").await.unwrap_err();
        assert!(
            err.to_string().contains("unknown oidc provider alias"),
            "{err}"
        );
    }
}
