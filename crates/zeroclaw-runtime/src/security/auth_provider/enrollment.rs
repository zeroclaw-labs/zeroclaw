//! Browserless OIDC enrollment: obtain an access token to present as
//! `auth_token` in the RPC handshake.
//!
//! Three standard flows against the issuer configured in `[oidc.<alias>]`:
//! - the Device Authorization Grant (RFC 8628) for interactive humans on a
//!   browserless host — start, show the user code and verification URI,
//!   poll until the IdP grants the token;
//! - the Authorization Code grant with PKCE (RFC 7636, S256 only) for hosts
//!   with a browser, paired with the RFC 8252 loopback listener; and
//! - `client_credentials` for headless service principals (requires the
//!   entry's `client_secret`).
//!
//! Enrollment is CLIENT-side machinery: it talks to the IdP, never to the
//! daemon, and nothing here stores the resulting token — the caller
//! presents it in `initialize` (or exports it as `ZEROCLAW_AUTH_TOKEN`).

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use zeroclaw_config::schema::OidcConfig;

#[derive(Debug, Clone, Deserialize)]
struct EnrollmentDiscovery {
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
    #[serde(default)]
    authorization_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Serialize exists for the gateway enrollment API, whose whole purpose
/// is returning the token to the enrolling client; Debug stays redacted.
#[derive(Clone, Serialize, Deserialize)]
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
    /// The user has not approved yet; poll again after the interval.
    Pending,
    /// The IdP asked to slow down; add five seconds to the interval.
    SlowDown,
    /// The token was granted.
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
        let response = self
            .http
            .post(&endpoint)
            .form(&[
                ("client_id", self.config.effective_client_id()),
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

    /// One poll of the token endpoint for an in-flight device grant.
    pub async fn device_grant_poll(&self, device_code: &str) -> Result<DevicePollOutcome> {
        let discovery = self.discovery().await?;
        let mut form = vec![
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", self.config.effective_client_id()),
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

    /// Confidential-client `client_credentials` grant for headless service
    /// principals. Requires the entry's `client_secret`.
    pub async fn client_credentials(&self) -> Result<EnrolledToken> {
        let Some(secret) = self.config.client_secret.as_deref() else {
            bail!("client_credentials enrollment requires oidc client_secret");
        };
        let discovery = self.discovery().await?;
        let response = self
            .http
            .post(&discovery.token_endpoint)
            .basic_auth(self.config.effective_client_id(), Some(secret))
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

/// An in-flight Authorization Code + PKCE attempt (RFC 7636, S256 only).
/// The verifier and nonce stay in process memory for the lifetime of one
/// attempt and leave it only inside the code exchange.
pub struct PkceFlow {
    /// The IdP authorize URL to open in a browser.
    pub authorize_url: String,
    /// The anti-CSRF state the callback must echo.
    pub state: String,
    pub(crate) nonce: String,
    pub(crate) verifier: String,
    pub(crate) redirect_uri: String,
    pub(crate) token_endpoint: String,
}

impl std::fmt::Debug for PkceFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PkceFlow")
            .field("authorize_url", &"<contains challenge; redacted>")
            .field("state", &self.state)
            .field("nonce", &"<redacted>")
            .field("verifier", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .field("token_endpoint", &self.token_endpoint)
            .finish()
    }
}

fn random_urlsafe(bytes: usize) -> Result<String> {
    use ring::rand::SecureRandom as _;
    let rng = ring::rand::SystemRandom::new();
    let mut buf = vec![0u8; bytes];
    rng.fill(&mut buf)
        .map_err(|_| anyhow::Error::msg("system randomness unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// RFC 7636 S256: BASE64URL-ENCODE(SHA256(ASCII(verifier))).
pub(crate) fn s256_challenge(verifier: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.as_ref())
}

/// The id_token riding along with a code exchange proves the authorize
/// round-trip: its nonce must be the one this flow sent. The id_token is
/// discarded afterwards; it is never presented as a credential (the
/// daemon rejects nonce-marked ID tokens outright).
fn verify_id_token_nonce(id_token: &str, expected: &str) -> Result<()> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow::Error::msg("id_token is not a JWT"))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| anyhow::Error::msg("id_token payload is not base64url"))?;
    let claims: serde_json::Value =
        serde_json::from_slice(&bytes).context("id_token payload is not JSON")?;
    match claims.get("nonce").and_then(|v| v.as_str()) {
        Some(nonce) if nonce == expected => Ok(()),
        _ => bail!(
            "id_token nonce does not match this flow (possible token substitution); \
             aborting enrollment"
        ),
    }
}

impl Enrollment {
    /// Begin an Authorization Code + PKCE flow (S256 only): returns the
    /// authorize URL to open in a browser plus the client-held secrets
    /// for the exchange. Refuses issuers that do not advertise S256
    /// (RFC 8414 defaults the advertisement to `plain` when the field is
    /// absent): there is no downgrade path.
    pub async fn pkce_start(&self, redirect_uri: &str) -> Result<PkceFlow> {
        let discovery = self.discovery().await?;
        let Some(authorize_endpoint) = discovery.authorization_endpoint else {
            bail!(
                "issuer {} does not advertise an authorization_endpoint; \
                 use device or client_credentials enrollment instead",
                self.config.issuer
            );
        };
        let s256_supported = discovery
            .code_challenge_methods_supported
            .as_deref()
            .is_some_and(|methods| methods.iter().any(|m| m == "S256"));
        if !s256_supported {
            bail!(
                "issuer {} does not advertise S256 PKCE support; refusing to \
                 enroll (plain PKCE is not an accepted downgrade)",
                self.config.issuer
            );
        }
        let verifier = random_urlsafe(32)?;
        let state = random_urlsafe(16)?;
        let nonce = random_urlsafe(16)?;
        let challenge = s256_challenge(&verifier);
        let authorize_url = reqwest::Url::parse_with_params(
            &authorize_endpoint,
            &[
                ("response_type", "code"),
                ("client_id", self.config.effective_client_id()),
                ("redirect_uri", redirect_uri),
                ("scope", "openid"),
                ("state", state.as_str()),
                ("nonce", nonce.as_str()),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
            ],
        )
        .context("authorization_endpoint is not a valid URL")?
        .to_string();
        Ok(PkceFlow {
            authorize_url,
            state,
            nonce,
            verifier,
            redirect_uri: redirect_uri.to_string(),
            token_endpoint: discovery.token_endpoint,
        })
    }

    /// Exchange the authorization code from the callback. When the
    /// response bundles an id_token, its nonce is verified against this
    /// flow before anything is returned.
    pub async fn pkce_exchange(&self, flow: &PkceFlow, code: &str) -> Result<EnrolledToken> {
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", flow.redirect_uri.as_str()),
            ("client_id", self.config.effective_client_id()),
            ("code_verifier", flow.verifier.as_str()),
        ];
        if let Some(secret) = self.config.client_secret.as_deref() {
            form.push(("client_secret", secret));
        }
        let response = self
            .http
            .post(&flow.token_endpoint)
            .form(&form)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let err: OAuthError = response
                .json()
                .await
                .with_context(|| format!("token endpoint returned HTTP {status}"))?;
            bail!(
                "authorization code exchange failed: {}{}",
                err.error,
                err.error_description
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            );
        }
        let raw: serde_json::Map<String, serde_json::Value> = response
            .json()
            .await
            .context("token response is not valid JSON")?;
        if let Some(id_token) = raw.get("id_token").and_then(|v| v.as_str()) {
            verify_id_token_nonce(id_token, &flow.nonce)?;
        }
        let token: EnrolledToken = serde_json::from_value(serde_json::Value::Object(raw))
            .context("token response is missing required fields")?;
        Ok(token)
    }
}

const SUCCESS_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Signed in</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Signed in</h1>\
<p>Enrollment is complete. You can close this tab and return to the terminal.</p>";

const FAILURE_PAGE: &str = "<!doctype html><meta charset=\"utf-8\"><title>Sign-in not completed</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Sign-in not completed</h1>\
<p>This request did not complete an enrollment. Return to the terminal and try again.</p>";

enum CallbackParse {
    Code(String),
    IdpError(String),
    Ignore,
}

/// The one-shot loopback callback listener (RFC 8252): binds an
/// ephemeral 127.0.0.1 port, answers exactly one matching callback, and
/// shuts down. Requests that do not carry this flow's state get a fixed
/// page and the wait continues; nothing from any request is echoed back.
pub struct LoopbackListener {
    listener: tokio::net::TcpListener,
    port: u16,
}

impl LoopbackListener {
    pub async fn bind() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("cannot bind a loopback callback port")?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    /// The redirect URI to start the flow with.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait for the callback carrying `expected_state`. An IdP `error`
    /// response for the matching state fails the flow; everything else
    /// (wrong path, wrong state, unparsable) is answered and ignored,
    /// bounded by `timeout`.
    pub async fn wait_for_code(self, expected_state: &str, timeout: Duration) -> Result<String> {
        match tokio::time::timeout(timeout, self.accept_loop(expected_state)).await {
            Ok(result) => result,
            Err(_) => bail!("timed out waiting for the browser sign-in to complete"),
        }
    }

    async fn accept_loop(&self, expected_state: &str) -> Result<String> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            let Ok(target) = Self::read_request_target(&mut stream).await else {
                continue;
            };
            match Self::parse_callback(&target, expected_state) {
                CallbackParse::Code(code) => {
                    Self::respond(&mut stream, 200, SUCCESS_PAGE).await;
                    return Ok(code);
                }
                CallbackParse::IdpError(err) => {
                    Self::respond(&mut stream, 200, FAILURE_PAGE).await;
                    bail!("the identity provider denied the sign-in: {err}");
                }
                CallbackParse::Ignore => {
                    Self::respond(&mut stream, 404, FAILURE_PAGE).await;
                }
            }
        }
    }

    async fn read_request_target(stream: &mut tokio::net::TcpStream) -> Result<String> {
        use tokio::io::AsyncReadExt as _;
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 512];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(2).any(|w| w == b"\r\n") || buf.len() > 8192 {
                break;
            }
        }
        let line = buf.split(|&b| b == b'\r').next().unwrap_or_default();
        let line = std::str::from_utf8(line)
            .map_err(|_| anyhow::Error::msg("request line is not UTF-8"))?;
        let mut parts = line.split(' ');
        if parts.next() != Some("GET") {
            bail!("not a GET request");
        }
        Ok(parts.next().unwrap_or_default().to_string())
    }

    fn parse_callback(target: &str, expected_state: &str) -> CallbackParse {
        let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
            return CallbackParse::Ignore;
        };
        if url.path() != "/callback" {
            return CallbackParse::Ignore;
        }
        let mut state = None;
        let mut code = None;
        let mut error = None;
        let mut error_description = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "state" => state = Some(value.into_owned()),
                "code" => code = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                "error_description" => error_description = Some(value.into_owned()),
                _ => {}
            }
        }
        // State gates everything: an error without this flow's state is
        // some other request's business, not a verdict on this flow.
        if state.as_deref() != Some(expected_state) {
            return CallbackParse::Ignore;
        }
        if let Some(err) = error {
            let detail = error_description
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            return CallbackParse::IdpError(format!("{err}{detail}"));
        }
        match code {
            Some(code) => CallbackParse::Code(code),
            None => CallbackParse::Ignore,
        }
    }

    async fn respond(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
        use tokio::io::AsyncWriteExt as _;
        let reason = if status == 200 { "OK" } else { "Not Found" };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
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
    async fn idp_with_pkce(methods: Option<serde_json::Value>) -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        let mut discovery = serde_json::json!({
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
        });
        if let Some(methods) = methods {
            discovery["code_challenge_methods_supported"] = methods;
        }
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn s256_challenge_matches_the_rfc_7636_vector() {
        assert_eq!(
            s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[tokio::test]
    async fn pkce_start_builds_an_s256_authorize_url() {
        let server = idp_with_pkce(Some(serde_json::json!(["plain", "S256"]))).await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        let flow = enrollment
            .pkce_start("http://127.0.0.1:7777/callback")
            .await
            .unwrap();
        let url = reqwest::Url::parse(&flow.authorize_url).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(params["response_type"], "code");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(
            params["code_challenge"],
            s256_challenge(&flow.verifier).as_str(),
            "the challenge commits to this flow's verifier"
        );
        assert_eq!(params["state"], flow.state.as_str());
        assert_eq!(params["redirect_uri"], "http://127.0.0.1:7777/callback");
        assert!(
            flow.verifier.len() >= 43,
            "RFC 7636 minimum verifier length"
        );
    }

    #[tokio::test]
    async fn pkce_start_refuses_issuers_without_s256() {
        for methods in [None, Some(serde_json::json!(["plain"]))] {
            let server = idp_with_pkce(methods).await;
            let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
            let err = enrollment
                .pkce_start("http://127.0.0.1:7777/callback")
                .await
                .unwrap_err();
            assert!(err.to_string().contains("S256"), "{err}");
        }
    }

    fn fake_id_token(nonce: &str) -> String {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({ "nonce": nonce }).to_string());
        format!("{header}.{payload}.sig")
    }

    #[tokio::test]
    async fn pkce_exchange_sends_the_verifier_and_checks_the_nonce() {
        let server = idp_with_pkce(Some(serde_json::json!(["S256"]))).await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        let flow = enrollment
            .pkce_start("http://127.0.0.1:7777/callback")
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains(format!(
                "code_verifier={}",
                flow.verifier
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-pkce",
                "expires_in": 3600,
                "id_token": fake_id_token(&flow.nonce),
            })))
            .mount(&server)
            .await;
        let token = enrollment
            .pkce_exchange(&flow, "auth-code-1")
            .await
            .unwrap();
        assert_eq!(token.access_token, "at-pkce");
    }

    #[tokio::test]
    async fn pkce_exchange_rejects_a_wrong_id_token_nonce() {
        let server = idp_with_pkce(Some(serde_json::json!(["S256"]))).await;
        let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
        let flow = enrollment
            .pkce_start("http://127.0.0.1:7777/callback")
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-substituted",
                "id_token": fake_id_token("some-other-flows-nonce"),
            })))
            .mount(&server)
            .await;
        let err = enrollment
            .pkce_exchange(&flow, "auth-code-1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nonce"), "{err}");
    }

    #[tokio::test]
    async fn loopback_listener_ignores_wrong_state_and_returns_the_matching_code() {
        let listener = LoopbackListener::bind().await.unwrap();
        let base = listener.redirect_uri();
        let wait = listener.wait_for_code("good-state", Duration::from_secs(5));
        let drive = async {
            // Wrong state: answered, ignored, the wait continues.
            let resp = reqwest::get(format!("{base}?code=evil&state=bad-state"))
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 404);
            // Matching state: the code comes back and the listener is done.
            let resp = reqwest::get(format!("{base}?code=real-code&state=good-state"))
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 200);
        };
        let (code, ()) = tokio::join!(wait, drive);
        assert_eq!(code.unwrap(), "real-code");
    }

    #[tokio::test]
    async fn loopback_listener_fails_the_flow_on_an_idp_error() {
        let listener = LoopbackListener::bind().await.unwrap();
        let base = listener.redirect_uri();
        let wait = listener.wait_for_code("good-state", Duration::from_secs(5));
        let drive = async {
            reqwest::get(format!(
                "{base}?error=access_denied&error_description=user+rejected&state=good-state"
            ))
            .await
            .unwrap();
        };
        let (result, ()) = tokio::join!(wait, drive);
        let err = result.unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");
        assert!(err.to_string().contains("user rejected"), "{err}");
    }
}
