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

    /// One poll, keeping RFC 8628 `slow_down` distinct from
    /// `authorization_pending`: the former obliges the caller to add five
    /// seconds to its interval, the latter to keep the current one.
    pub async fn device_poll(&self, alias: &str, device_code: &str) -> Result<DevicePoll> {
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
                Some(token) => Ok(DevicePoll::Granted(token.access_token)),
                None => bail!("gateway reported a grant without a token"),
            },
            "pending" => Ok(DevicePoll::Pending),
            "slow_down" => Ok(DevicePoll::SlowDown),
            other => bail!("unexpected poll status from the gateway: {other}"),
        }
    }
}

/// One device-grant poll outcome. RFC 8628 §3.5 treats `slow_down` and
/// `authorization_pending` differently, so the two must stay distinct.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DevicePoll {
    Granted(String),
    Pending,
    SlowDown,
}

/// RFC 8628 §3.5 interval update. The client must never poll faster than
/// the advertised `minimum`; `slow_down` obliges it to add five seconds,
/// while `pending` keeps the current wait. The result is never capped
/// below the floor, so an IdP advertising 60s is honored and a throttled
/// client backs off instead of polling early until the code expires.
fn next_poll_interval(current: u64, minimum: u64, outcome: &DevicePoll) -> u64 {
    let next = match outcome {
        DevicePoll::SlowDown => current.saturating_add(5),
        DevicePoll::Pending | DevicePoll::Granted(_) => current,
    };
    next.max(minimum)
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
    let minimum = start.interval.max(1);
    let mut interval = minimum;
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("the device code expired before approval");
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let outcome = gateway.device_poll(alias, &start.device_code).await?;
        if let DevicePoll::Granted(token) = outcome {
            return Ok(token);
        }
        interval = next_poll_interval(interval, minimum, &outcome);
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
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("throttled"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "status": "slow_down" })),
            )
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), false).unwrap();
        assert_eq!(
            gateway.device_poll("corp", "waiting").await.unwrap(),
            DevicePoll::Pending
        );
        // slow_down must survive as its own outcome: folding it into
        // pending would lose the RFC 8628 §3.5 back-off obligation.
        assert_eq!(
            gateway.device_poll("corp", "throttled").await.unwrap(),
            DevicePoll::SlowDown
        );
        assert_eq!(
            gateway.device_poll("corp", "done").await.unwrap(),
            DevicePoll::Granted("at-tui".into())
        );
    }

    #[test]
    fn poll_interval_follows_rfc_8628() {
        // slow_down adds five seconds (5 -> 10), never one.
        assert_eq!(next_poll_interval(5, 5, &DevicePoll::SlowDown), 10);
        // Repeated throttling keeps backing off and is not capped at 30.
        assert_eq!(next_poll_interval(30, 5, &DevicePoll::SlowDown), 35);
        // pending keeps the current wait rather than growing it.
        assert_eq!(next_poll_interval(10, 5, &DevicePoll::Pending), 10);
        // An advertised interval above 30 is honored, never shrunk.
        assert_eq!(next_poll_interval(60, 60, &DevicePoll::Pending), 60);
        // The advertised minimum is a floor the interval never drops below.
        assert_eq!(next_poll_interval(1, 5, &DevicePoll::Pending), 5);
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
