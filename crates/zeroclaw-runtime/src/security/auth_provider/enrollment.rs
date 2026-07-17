use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use zeroclaw_config::schema::OidcConfig;

#[derive(Debug, Clone, Deserialize)]
struct EnrollmentDiscovery {
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceGrantStart {
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

#[derive(Clone, Deserialize)]
pub struct EnrolledToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for EnrolledToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledToken")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug)]
pub enum DevicePollOutcome {
    Pending,
    SlowDown,
    Token(Box<EnrolledToken>),
}

pub struct Enrollment {
    config: OidcConfig,
    http: reqwest::Client,
}

impl Enrollment {
    pub fn new(config: OidcConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { config, http })
    }

    async fn discovery(&self) -> Result<EnrollmentDiscovery> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        self.http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("issuer discovery document is not valid JSON")
    }

    pub async fn device_grant_start(&self) -> Result<DeviceGrantStart> {
        let discovery = self.discovery().await?;
        let Some(endpoint) = discovery.device_authorization_endpoint else {
            bail!(
                "issuer {} does not advertise a device_authorization_endpoint; \
                 use client_credentials enrollment instead",
                self.config.issuer
            );
        };
        let response = self
            .http
            .post(&endpoint)
            .form(&[
                ("client_id", self.config.enrollment_client_id()),
                ("scope", "openid"),
            ])
            .send()
            .await?
            .error_for_status()?;
        response
            .json()
            .await
            .context("device authorization response is not valid JSON")
    }

    pub async fn device_grant_poll(&self, device_code: &str) -> Result<DevicePollOutcome> {
        let discovery = self.discovery().await?;
        let mut form = vec![
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", self.config.enrollment_client_id()),
        ];
        if let Some(secret) = self.config.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        let response = self
            .http
            .post(&discovery.token_endpoint)
            .form(&form)
            .send()
            .await?;
        if response.status().is_success() {
            let token: EnrolledToken = response
                .json()
                .await
                .context("token response is not valid JSON")?;
            return Ok(DevicePollOutcome::Token(Box::new(token)));
        }
        let err: OAuthError = response
            .json()
            .await
            .context("OAuth error response is not valid JSON")?;
        match err.error.as_str() {
            "authorization_pending" => Ok(DevicePollOutcome::Pending),
            "slow_down" => Ok(DevicePollOutcome::SlowDown),
            other => bail!(
                "device grant failed: {other}{}",
                err.error_description
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            ),
        }
    }

    pub async fn client_credentials(&self) -> Result<EnrolledToken> {
        let Some(secret) = self.config.client_secret.as_deref() else {
            bail!("client_credentials enrollment requires oidc client_secret");
        };
        let discovery = self.discovery().await?;
        let response = self
            .http
            .post(&discovery.token_endpoint)
            .basic_auth(self.config.enrollment_client_id(), Some(secret))
            .form(&[("grant_type", "client_credentials"), ("scope", "openid")])
            .send()
            .await?;
        if response.status().is_success() {
            return response
                .json()
                .await
                .context("token response is not valid JSON");
        }
        let status = response.status();
        let err: OAuthError = response
            .json()
            .await
            .with_context(|| format!("token endpoint returned HTTP {status}"))?;
        bail!(
            "client_credentials enrollment failed: {}{}",
            err.error,
            err.error_description
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn idp_with_device_endpoint() -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_authorization_endpoint": format!("{issuer}/device"),
                "token_endpoint": format!("{issuer}/token"),
            })))
            .mount(&server)
            .await;
        server
    }

    fn config(issuer: &str, secret: Option<&str>) -> OidcConfig {
        OidcConfig {
            issuer: issuer.to_string(),
            audience: "zeroclaw".into(),
            client_id: "zerocode-cli".into(),
            client_secret: secret.map(str::to_owned),
            claim_path: "groups".into(),
            role_map: std::iter::once(("ops".to_string(), "operator".to_string())).collect(),
            ..OidcConfig::default()
        }
    }

    #[tokio::test]
    async fn device_grant_start_returns_user_code() {
        let server = idp_with_device_endpoint().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .and(body_string_contains("client_id=zerocode-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-123",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        let start = enrollment.device_grant_start().await.unwrap();
        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(start.device_code, "dev-123");
    }

    #[tokio::test]
    async fn device_grant_start_fails_without_endpoint() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_endpoint": format!("{issuer}/token"),
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&issuer, None)).unwrap();
        let err = enrollment.device_grant_start().await.unwrap_err();
        assert!(err.to_string().contains("device_authorization_endpoint"));
    }

    #[tokio::test]
    async fn device_grant_poll_maps_pending_and_token() {
        let server = idp_with_device_endpoint().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("device_code=pending-code"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("device_code=granted-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-42",
                "refresh_token": "rt-42",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        assert!(matches!(
            enrollment.device_grant_poll("pending-code").await.unwrap(),
            DevicePollOutcome::Pending
        ));
        match enrollment.device_grant_poll("granted-code").await.unwrap() {
            DevicePollOutcome::Token(token) => {
                assert_eq!(token.access_token, "at-42");
                assert_eq!(token.refresh_token.as_deref(), Some("rt-42"));
            }
            _ => panic!("expected token"),
        }
    }

    #[tokio::test]
    async fn device_grant_poll_denied_is_an_error() {
        let server = idp_with_device_endpoint().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied",
                "error_description": "user rejected the request",
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        let err = enrollment.device_grant_poll("dev-x").await.unwrap_err();
        assert!(err.to_string().contains("access_denied"));
        assert!(err.to_string().contains("user rejected"));
    }

    #[tokio::test]
    async fn client_credentials_requires_secret() {
        let server = idp_with_device_endpoint().await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        let err = enrollment.client_credentials().await.unwrap_err();
        assert!(err.to_string().contains("client_secret"));
    }

    #[tokio::test]
    async fn client_credentials_returns_token() {
        let server = idp_with_device_endpoint().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=client_credentials"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "svc-token",
                "expires_in": 300,
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&server.uri(), Some("s3cret"))).unwrap();
        let token = enrollment.client_credentials().await.unwrap();
        assert_eq!(token.access_token, "svc-token");
    }
}

#[cfg(test)]
mod debug_redaction {
    use super::*;

    #[test]
    fn enrolled_token_debug_redacts_secrets() {
        let token = EnrolledToken {
            access_token: "raw-access".into(),
            refresh_token: Some("raw-refresh".into()),
            expires_in: Some(60),
        };
        let dbg = format!("{token:?}");
        assert!(!dbg.contains("raw-access"));
        assert!(!dbg.contains("raw-refresh"));
        assert!(dbg.contains("<redacted>"));
    }
}
