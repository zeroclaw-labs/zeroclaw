//! Browserless OIDC enrollment: obtain an access token to present as
//! `auth_token` in the RPC handshake.
//!
//! Two standard flows against the issuer configured in `[oidc.<alias>]`:
//! - the Device Authorization Grant (RFC 8628) for interactive humans on a
//!   browserless host — start, show the user code and verification URI,
//!   poll until the IdP grants the token; and
//! - `client_credentials` for headless service principals (requires the
//!   entry's `client_secret`).
//!
//! Enrollment is CLIENT-side machinery: it talks to the IdP, never to the
//! daemon, and nothing here stores the resulting token — the caller
//! presents it in `initialize` (or exports it as `ZEROCLAW_AUTH_TOKEN`).

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use zeroclaw_config::schema::OidcConfig;

use super::oidc::{read_response_limited, validate_discovered_endpoint};

#[derive(Debug, Clone, Deserialize)]
struct EnrollmentDiscovery {
    /// The issuer the document asserts. Required to equal the configured
    /// issuer exactly before any advertised endpoint is used.
    #[serde(default)]
    issuer: Option<String>,
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
}

#[derive(Clone, Deserialize)]
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

impl std::fmt::Debug for DeviceGrantStart {
    /// The device code redeems the grant and the complete URI embeds the
    /// user code; neither belongs in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceGrantStart")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field(
                "verification_uri_complete",
                &self
                    .verification_uri_complete
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Clone, Deserialize)]
pub struct EnrolledToken {
    pub access_token: String,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl EnrolledToken {
    /// A token response is advertised as success only when it carries a
    /// non-empty access token of a type the RPC handshake can present
    /// (`Bearer`, RFC 6750). Anything else is refused here, before a caller
    /// prints or exports it.
    fn validated(self) -> Result<Self> {
        if self.access_token.trim().is_empty() {
            bail!("token response carries an empty access_token");
        }
        match self.token_type.as_deref() {
            Some(kind) if kind.eq_ignore_ascii_case("bearer") => Ok(self),
            Some(other) => bail!(
                "token response has unsupported token_type {other:?}; only Bearer tokens can be presented"
            ),
            None => bail!("token response has no token_type; only Bearer tokens can be presented"),
        }
    }
}

impl std::fmt::Debug for EnrolledToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledToken")
            .field("access_token", &"<redacted>")
            .field("token_type", &self.token_type)
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
    /// The user has not approved yet; poll again after the interval.
    Pending,
    /// The IdP asked to slow down; add five seconds to the interval.
    SlowDown,
    /// The token was granted.
    Token(Box<EnrolledToken>),
}

pub struct Enrollment {
    alias: String,
    config: OidcConfig,
    http: reqwest::Client,
}

impl Enrollment {
    /// Build an enrollment client for the `[oidc.<alias>]` entry.
    ///
    /// The entry is validated first (the same canonical issuer policy the
    /// daemon enforces: `https`, or `http` for an exact loopback host), and
    /// the HTTP client never follows a redirect: a credential-bearing
    /// request is sent to exactly the endpoint that was validated, and a
    /// redirect answer is refused rather than forwarded anywhere.
    pub fn new(alias: &str, config: OidcConfig) -> Result<Self> {
        config
            .validate(alias)
            .with_context(|| format!("[oidc.{alias}] is not a valid enrollment target"))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            alias: alias.to_string(),
            config,
            http,
        })
    }

    /// A redirect is never followed: refuse it here so no caller can treat
    /// the 3xx as anything but a failure.
    fn refuse_redirect(response: reqwest::Response, what: &str) -> Result<reqwest::Response> {
        if response.status().is_redirection() {
            bail!(
                "{what} answered with a redirect ({}); enrollment never follows redirects",
                response.status()
            );
        }
        Ok(response)
    }

    /// Confidential clients authenticate with HTTP Basic on every token- and
    /// device-endpoint request (RFC 6749 §2.3.1); public clients send their
    /// `client_id` in the form. One rule for device start, device poll and
    /// client_credentials alike.
    fn with_client_auth(
        &self,
        request: reqwest::RequestBuilder,
        form: &mut Vec<(&'static str, String)>,
    ) -> reqwest::RequestBuilder {
        match self.config.client_secret.as_deref() {
            Some(secret) => request.basic_auth(self.config.effective_client_id(), Some(secret)),
            None => {
                form.push(("client_id", self.config.effective_client_id().to_string()));
                request
            }
        }
    }

    /// Fetch and bind the issuer's discovery document.
    ///
    /// The document must assert exactly the configured issuer, or the
    /// metadata is somebody else's (or a spoofed or misrouted endpoint) and
    /// none of its endpoints may be used. Every endpoint that will receive
    /// a credential is then held to the canonical URL policy before any
    /// request is built. Re-run on every operation, so a poll after a
    /// changed document is re-bound too.
    async fn discovery(&self) -> Result<EnrollmentDiscovery> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let response = self.http.get(&url).send().await?;
        let response = Self::refuse_redirect(response, "issuer discovery")?.error_for_status()?;
        let body = read_response_limited(response).await?;
        let discovery: EnrollmentDiscovery =
            serde_json::from_slice(&body).context("issuer discovery document is not valid JSON")?;
        if discovery.issuer.as_deref() != Some(self.config.issuer.as_str()) {
            bail!(
                "discovery document issuer does not match the configured issuer for oidc.{}; \
                 refusing its endpoints",
                self.alias
            );
        }
        validate_discovered_endpoint(&discovery.token_endpoint, "token_endpoint")?;
        if let Some(endpoint) = discovery.device_authorization_endpoint.as_deref() {
            validate_discovered_endpoint(endpoint, "device_authorization_endpoint")?;
        }
        Ok(discovery)
    }

    /// Start the Device Authorization Grant: returns the user code and
    /// verification URI to show the human, plus the polling parameters.
    pub async fn device_grant_start(&self) -> Result<DeviceGrantStart> {
        let discovery = self.discovery().await?;
        let Some(endpoint) = discovery.device_authorization_endpoint else {
            bail!(
                "issuer {} does not advertise a device_authorization_endpoint; \
                 use client_credentials enrollment instead",
                self.config.issuer
            );
        };
        let mut form = vec![("scope", "openid".to_string())];
        let request = self.with_client_auth(self.http.post(&endpoint), &mut form);
        let response = request.form(&form).send().await?;
        let response =
            Self::refuse_redirect(response, "device authorization endpoint")?.error_for_status()?;
        let body = read_response_limited(response).await?;
        let start: DeviceGrantStart = serde_json::from_slice(&body)
            .context("device authorization response is not valid JSON")?;
        if start.expires_in == 0 {
            bail!("device authorization response advertises an already-expired code");
        }
        Ok(start)
    }

    /// One poll of the token endpoint for an in-flight device grant.
    pub async fn device_grant_poll(&self, device_code: &str) -> Result<DevicePollOutcome> {
        let discovery = self.discovery().await?;
        let mut form = vec![
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
            ("device_code", device_code.to_string()),
        ];
        let request = self.with_client_auth(self.http.post(&discovery.token_endpoint), &mut form);
        let response = request.form(&form).send().await?;
        let response = Self::refuse_redirect(response, "token endpoint")?;
        if response.status().is_success() {
            let body = read_response_limited(response).await?;
            let token: EnrolledToken =
                serde_json::from_slice(&body).context("token response is not valid JSON")?;
            return Ok(DevicePollOutcome::Token(Box::new(token.validated()?)));
        }
        let body = read_response_limited(response).await?;
        let err: OAuthError =
            serde_json::from_slice(&body).context("OAuth error response is not valid JSON")?;
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

    /// Confidential-client `client_credentials` grant for headless service
    /// principals. Requires the entry's `client_secret`.
    pub async fn client_credentials(&self) -> Result<EnrolledToken> {
        if self.config.client_secret.is_none() {
            bail!("client_credentials enrollment requires oidc client_secret");
        }
        let discovery = self.discovery().await?;
        let mut form = vec![
            ("grant_type", "client_credentials".to_string()),
            ("scope", "openid".to_string()),
        ];
        let request = self.with_client_auth(self.http.post(&discovery.token_endpoint), &mut form);
        let response = request.form(&form).send().await?;
        let response = Self::refuse_redirect(response, "token endpoint")?;
        let status = response.status();
        let body = read_response_limited(response).await?;
        if status.is_success() {
            let token: EnrolledToken =
                serde_json::from_slice(&body).context("token response is not valid JSON")?;
            return token.validated();
        }
        let err: OAuthError = serde_json::from_slice(&body)
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
    use std::collections::HashMap;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn idp_with_device_endpoint() -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "device_authorization_endpoint": format!("{issuer}/device"),
                "token_endpoint": format!("{issuer}/token"),
            })))
            .mount(&server)
            .await;
        server
    }

    /// An IdP whose discovery document is `document`, with the device and
    /// token endpoints mounted and asserting that they are never reached.
    async fn idp_with_document(document: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(document))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn discovery_without_the_configured_issuer_uses_no_endpoint() {
        // Missing issuer, and a foreign issuer: neither document's endpoints
        // receive a single request, for the public and the confidential flow.
        for document in [
            |issuer: &str| {
                serde_json::json!({
                    "device_authorization_endpoint": format!("{issuer}/device"),
                    "token_endpoint": format!("{issuer}/token"),
                })
            },
            |issuer: &str| {
                serde_json::json!({
                    "issuer": "https://evil.example.com",
                    "device_authorization_endpoint": format!("{issuer}/device"),
                    "token_endpoint": format!("{issuer}/token"),
                })
            },
        ] {
            let server = MockServer::start().await;
            let issuer = server.uri();
            let server_with_doc = idp_with_document(document(&issuer)).await;
            let issuer = server_with_doc.uri();
            drop(server);
            for secret in [None, Some("s3cr3t")] {
                let enrollment = Enrollment::new("corp", config(&issuer, secret)).unwrap();
                let err = enrollment.device_grant_start().await.unwrap_err();
                assert!(err.to_string().contains("issuer"), "{err}");
                let err = enrollment.device_grant_poll("dev-1").await.unwrap_err();
                assert!(err.to_string().contains("issuer"), "{err}");
                if secret.is_some() {
                    let err = enrollment.client_credentials().await.unwrap_err();
                    assert!(err.to_string().contains("issuer"), "{err}");
                }
            }
            // Dropping the server verifies the `.expect(0)` on both endpoints.
            drop(server_with_doc);
        }
    }

    #[tokio::test]
    async fn advertised_endpoints_are_held_to_the_canonical_url_policy() {
        // An issuer that asserts itself correctly but advertises a remote
        // http endpoint, a lookalike loopback host, or a userinfo-carrying
        // URL: refused before any credential-bearing request is built.
        for bad in [
            "http://sso.example.com/token",
            "http://localhost.evil.example/token",
            "http://127.0.0.1.evil.example/token",
            "https://user:pass@sso.example.com/token",
        ] {
            let server = MockServer::start().await;
            let issuer = server.uri();
            Mock::given(method("GET"))
                .and(path("/.well-known/openid-configuration"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issuer": issuer,
                    "device_authorization_endpoint": bad,
                    "token_endpoint": bad,
                })))
                .mount(&server)
                .await;
            let enrollment = Enrollment::new("corp", config(&issuer, Some("s3cr3t"))).unwrap();
            let err = enrollment.client_credentials().await.unwrap_err();
            assert!(err.to_string().contains("token_endpoint"), "{bad}: {err}");
            let err = enrollment.device_grant_start().await.unwrap_err();
            assert!(err.to_string().contains("endpoint"), "{bad}: {err}");
        }
    }

    #[tokio::test]
    async fn redirects_are_never_followed_for_enrollment_requests() {
        // The token endpoint answers with a redirect to another path on the
        // same origin (a same-origin downgrade is the friendliest case);
        // the redirect target must see zero requests, and the discovery
        // fetch itself refuses a redirect too.
        let server = idp_with_device_endpoint().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/token-elsewhere"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token-elsewhere"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let enrollment = Enrollment::new("corp", config(&server.uri(), Some("s3cr3t"))).unwrap();
        let err = enrollment.client_credentials().await.unwrap_err();
        assert!(err.to_string().contains("redirect"), "{err}");
        let err = enrollment.device_grant_poll("dev-1").await.unwrap_err();
        assert!(err.to_string().contains("redirect"), "{err}");

        let redirecting = MockServer::start().await;
        let issuer = redirecting.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", format!("{issuer}/elsewhere")),
            )
            .mount(&redirecting)
            .await;
        let enrollment = Enrollment::new("corp", config(&issuer, None)).unwrap();
        let err = enrollment.device_grant_start().await.unwrap_err();
        assert!(err.to_string().contains("redirect"), "{err}");
    }

    #[tokio::test]
    async fn confidential_clients_authenticate_with_basic_on_every_endpoint() {
        use wiremock::matchers::header_exists;
        let server = idp_with_device_endpoint().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-1",
                "user_code": "AAAA-BBBB",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(header_exists("authorization"))
            .and(body_string_contains("device_code=dev-1"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "authorization_pending",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let enrollment = Enrollment::new("corp", config(&server.uri(), Some("s3cr3t"))).unwrap();
        let start = enrollment.device_grant_start().await.unwrap();
        assert!(
            !format!("{start:?}").contains("dev-1"),
            "the device code is redacted from Debug"
        );
        assert!(matches!(
            enrollment.device_grant_poll("dev-1").await.unwrap(),
            DevicePollOutcome::Pending
        ));
    }

    #[tokio::test]
    async fn token_responses_are_validated_before_success_is_advertised() {
        for (body, expected) in [
            (
                serde_json::json!({"access_token": "", "token_type": "Bearer"}),
                "empty access_token",
            ),
            (
                serde_json::json!({"access_token": "t", "token_type": "MAC"}),
                "unsupported token_type",
            ),
            (serde_json::json!({"access_token": "t"}), "no token_type"),
        ] {
            let server = idp_with_device_endpoint().await;
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
            let enrollment =
                Enrollment::new("corp", config(&server.uri(), Some("s3cr3t"))).unwrap();
            let err = enrollment.client_credentials().await.unwrap_err();
            assert!(err.to_string().contains(expected), "{expected}: {err}");
            let err = enrollment.device_grant_poll("dev-1").await.unwrap_err();
            assert!(err.to_string().contains(expected), "{expected}: {err}");
        }
    }

    #[tokio::test]
    async fn an_invalid_entry_is_refused_at_construction() {
        let mut bad = config("https://sso.example.com", None);
        bad.audience = String::new();
        let err = match Enrollment::new("corp", bad) {
            Err(err) => format!("{err:#}"),
            Ok(_) => panic!("an entry without an audience must be refused"),
        };
        assert!(err.contains("audience"), "{err}");
        let err = match Enrollment::new("corp", config("http://sso.example.com", None)) {
            Err(err) => format!("{err:#}"),
            Ok(_) => panic!("a remote http issuer must be refused"),
        };
        assert!(err.contains("https"), "{err}");
    }

    fn config(issuer: &str, secret: Option<&str>) -> OidcConfig {
        OidcConfig {
            issuer: issuer.to_string(),
            audience: "zeroclaw".into(),
            client_id: "zerocode-cli".into(),
            client_secret: secret.map(str::to_owned),
            claim_path: "groups".into(),
            profile_map: HashMap::from([("ops".to_string(), "operator".to_string())]),
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
        let enrollment = Enrollment::new("corp", config(&server.uri(), None)).unwrap();
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
                "issuer": issuer,
                "token_endpoint": format!("{issuer}/token"),
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new("corp", config(&issuer, None)).unwrap();
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
                "token_type": "Bearer",
                "refresh_token": "rt-42",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new("corp", config(&server.uri(), None)).unwrap();
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
        let enrollment = Enrollment::new("corp", config(&server.uri(), None)).unwrap();
        let err = enrollment.device_grant_poll("dev-x").await.unwrap_err();
        assert!(err.to_string().contains("access_denied"));
        assert!(err.to_string().contains("user rejected"));
    }

    #[tokio::test]
    async fn client_credentials_requires_secret() {
        let server = idp_with_device_endpoint().await;
        let enrollment = Enrollment::new("corp", config(&server.uri(), None)).unwrap();
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
                "token_type": "bearer",
                "expires_in": 300,
            })))
            .mount(&server)
            .await;
        let enrollment = Enrollment::new("corp", config(&server.uri(), Some("s3cret"))).unwrap();
        let token = enrollment.client_credentials().await.unwrap();
        assert_eq!(token.access_token, "svc-token");
    }

    #[test]
    fn enrolled_token_debug_redacts_secrets() {
        let token = EnrolledToken {
            access_token: "raw-access".into(),
            token_type: Some("Bearer".into()),
            refresh_token: Some("raw-refresh".into()),
            expires_in: Some(60),
        };
        let dbg = format!("{token:?}");
        assert!(!dbg.contains("raw-access"));
        assert!(!dbg.contains("raw-refresh"));
        assert!(dbg.contains("<redacted>"));
    }
}
