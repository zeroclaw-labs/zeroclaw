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

use super::oidc::{read_response_limited, validate_discovered_endpoint};

#[derive(Debug, Clone, Deserialize)]
struct EnrollmentDiscovery {
    /// The identifier the document asserts for itself. Optional here only so
    /// that its absence is refused with a specific error rather than a serde
    /// field error; `Enrollment::discovery` requires it.
    #[serde(default)]
    issuer: Option<String>,
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
    #[serde(default)]
    authorization_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Option<Vec<String>>,
}

#[derive(Clone, Serialize, Deserialize)]
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

/// `device_code` is the bearer credential of an in-flight device grant —
/// whoever holds it can redeem the user's approval — so it is redacted here.
/// Everything else is shown to the user by the enrolling client anyway and
/// stays visible, which is what makes a trace of this struct worth having.
impl std::fmt::Debug for DeviceGrantStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceGrantStart")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("verification_uri_complete", &self.verification_uri_complete)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
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
    /// The IdP rejected the grant outright (`access_denied`,
    /// `expired_token`, an unknown device code, and the like): the OAuth
    /// error with its description. Distinct from a transport or parse
    /// failure, which is an `Err` and says nothing about the device code.
    Denied(String),
    /// The token was granted.
    Token(Box<EnrolledToken>),
}

pub struct Enrollment {
    config: OidcConfig,
    http: reqwest::Client,
}

impl Enrollment {
    pub fn new(config: OidcConfig) -> Result<Self> {
        // No redirects, the rule the verification sibling already applies.
        // reqwest follows up to ten by default and re-sends the body on 307
        // and 308, so a redirecting token, device or authorization endpoint
        // could carry the client secret, the PKCE verifier or the device
        // code to another origin, in cleartext at that, after discovery had
        // already checked the transport of the endpoint it was handed.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { config, http })
    }

    /// Fetch and verify the issuer's metadata document.
    ///
    /// Two checks run here, before any endpoint leaves this function, so every
    /// flow built on it (device start, device poll, `client_credentials`, PKCE
    /// start) is covered by construction.
    ///
    /// The issuer check binds the document's identity. RFC 8414 section 3.3
    /// (and OIDC Discovery 4.3): the `issuer` the document asserts must be
    /// byte-identical to the identifier the client started from. A document
    /// that names another identifier is not this issuer's metadata, whatever
    /// URL served it, so none of its endpoints may be used.
    ///
    /// The endpoint check binds the document's transport. Each endpoint the
    /// enrollment flows post to — the token endpoint always, the authorization
    /// and device authorization endpoints when advertised — must be `https`,
    /// or `http` on an exact loopback host for a local IdP, under the same rule
    /// the verifier sibling applies to the endpoints it discovers. Without it a
    /// document with the right identifier could still put the client secret,
    /// the PKCE verifier and the device code on the wire in cleartext.
    ///
    /// Returns the document together with that verified issuer identifier.
    /// The body is read through the bounded reader shared with the verifier
    /// sibling: an IdP document is untrusted input and is capped rather than
    /// buffered to whatever length the peer likes.
    async fn discovery(&self) -> Result<(EnrollmentDiscovery, String)> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let body = read_response_limited(self.http.get(&url).send().await?.error_for_status()?)
            .await
            .context("issuer discovery document could not be read")?;
        let discovery: EnrollmentDiscovery =
            serde_json::from_slice(&body).context("issuer discovery document is not valid JSON")?;
        let Some(issuer) = discovery.issuer.clone() else {
            bail!(
                "the discovery document for {} has no issuer; refusing to use its endpoints",
                self.config.issuer
            );
        };
        if issuer != self.config.issuer {
            bail!(
                "the discovery document asserts issuer {issuer} but this alias is configured \
                 for {}; the configured issuer must match the document's identifier exactly, \
                 including any trailing slash",
                self.config.issuer
            );
        }
        // The identifier is right; now the endpoints it hands out have to be
        // safe to post credentials to. Refusing here keeps the request from
        // ever being built, for whichever flow asked for the document.
        validate_discovered_endpoint(&discovery.token_endpoint, "token_endpoint")?;
        if let Some(endpoint) = discovery.authorization_endpoint.as_deref() {
            validate_discovered_endpoint(endpoint, "authorization_endpoint")?;
        }
        if let Some(endpoint) = discovery.device_authorization_endpoint.as_deref() {
            validate_discovered_endpoint(endpoint, "device_authorization_endpoint")?;
        }
        Ok((discovery, issuer))
    }

    /// Start the Device Authorization Grant: returns the user code and
    /// verification URI to show the human, plus the polling parameters.
    pub async fn device_grant_start(&self) -> Result<DeviceGrantStart> {
        let (discovery, _issuer) = self.discovery().await?;
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
        let body = read_response_limited(response)
            .await
            .context("device authorization response could not be read")?;
        serde_json::from_slice(&body).context("device authorization response is not valid JSON")
    }

    /// One poll of the token endpoint for an in-flight device grant.
    pub async fn device_grant_poll(&self, device_code: &str) -> Result<DevicePollOutcome> {
        let (discovery, _issuer) = self.discovery().await?;
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
        let success = response.status().is_success();
        let body = read_response_limited(response)
            .await
            .context("token response could not be read")?;
        if success {
            let token: EnrolledToken =
                serde_json::from_slice(&body).context("token response is not valid JSON")?;
            return Ok(DevicePollOutcome::Token(Box::new(token)));
        }
        let err: OAuthError =
            serde_json::from_slice(&body).context("OAuth error response is not valid JSON")?;
        match err.error.as_str() {
            "authorization_pending" => Ok(DevicePollOutcome::Pending),
            "slow_down" => Ok(DevicePollOutcome::SlowDown),
            other => Ok(DevicePollOutcome::Denied(format!(
                "{other}{}",
                err.error_description
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            ))),
        }
    }

    /// Confidential-client `client_credentials` grant for headless service
    /// principals. Requires the entry's `client_secret`.
    pub async fn client_credentials(&self) -> Result<EnrolledToken> {
        let Some(secret) = self.config.client_secret.as_deref() else {
            bail!("client_credentials enrollment requires oidc client_secret");
        };
        let (discovery, _issuer) = self.discovery().await?;
        let response = self
            .http
            .post(&discovery.token_endpoint)
            .basic_auth(self.config.effective_client_id(), Some(secret))
            .form(&[("grant_type", "client_credentials"), ("scope", "openid")])
            .send()
            .await?;
        let status = response.status();
        let body = read_response_limited(response)
            .await
            .context("token response could not be read")?;
        if status.is_success() {
            return serde_json::from_slice(&body).context("token response is not valid JSON");
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
    /// The issuer and client this flow was started for. `issuer` is the
    /// identifier the discovery document asserted, verified at start to be
    /// exactly the configured one (the two are byte-equal once that check
    /// passes), so it names the provider whose `token_endpoint` is pinned
    /// above rather than merely what the config said. The code exchange posts
    /// credentials to that endpoint; binding the initiating issuer/client lets
    /// the exchange refuse to run if the alias was repointed to a different
    /// provider while the flow was pending, so a live A->B swap can never send
    /// B's client secret to A's endpoint.
    pub(crate) issuer: String,
    pub(crate) client_id: String,
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
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .finish()
    }
}

impl PkceFlow {
    /// RFC 9207 mix-up defense shared by both callback adapters (the CLI
    /// loopback listener and the gateway callback): when the authorization
    /// response carries an `iss` parameter it must name exactly the issuer
    /// this flow was started for, and the check runs before the response's
    /// code or error is acted on. A response without `iss` is accepted for
    /// providers that predate RFC 9207; that is the only leniency.
    pub fn check_callback_issuer(&self, iss: Option<&str>) -> Result<()> {
        match iss {
            None => Ok(()),
            Some(iss) if iss == self.issuer => Ok(()),
            Some(iss) => bail!(
                "the authorization response names issuer {iss} but this sign-in was \
                 started with {}; refusing the response (possible mix-up attack)",
                self.issuer
            ),
        }
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

/// Clock skew tolerated when checking the id_token's `exp`.
const ID_TOKEN_CLOCK_LEEWAY_SECS: u64 = 30;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Validate the id_token bundled with a code exchange against this flow
/// (OIDC Core 3.1.3.7): the token must be a three-segment JWT whose claims
/// name this flow's issuer, include this client in `aud` (with any `azp`
/// equal to the client, and an `azp` REQUIRED when `aud` names more than one
/// party), have not expired, and echo this flow's `nonce`.
/// The signature is not checked separately: the token arrived over the
/// TLS-verified token endpoint in a direct exchange, the case 3.1.3.7 step
/// 6 exempts. The id_token is discarded afterwards; it is never presented
/// as a credential (the daemon rejects nonce-marked ID tokens outright).
fn validate_id_token(id_token: &str, flow: &PkceFlow, now: u64) -> Result<()> {
    let mut segments = id_token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        bail!("id_token is not a three-segment JWT; refusing the token response");
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| anyhow::Error::msg("id_token payload is not base64url"))?;
    let claims: serde_json::Value =
        serde_json::from_slice(&bytes).context("id_token payload is not JSON")?;
    let Some(claims) = claims.as_object() else {
        bail!("id_token payload is not a JSON object");
    };

    match claims.get("iss").and_then(|v| v.as_str()) {
        Some(iss) if iss == flow.issuer => {}
        Some(iss) => bail!(
            "id_token issuer {iss} does not match this flow's issuer {}; aborting enrollment",
            flow.issuer
        ),
        None => bail!("id_token carries no issuer claim; aborting enrollment"),
    }

    // OIDC Core 3.1.3.7 step 3: `aud` must name this client. Step 4: when it
    // names more than one party, `azp` is REQUIRED and must be this client —
    // a token minted for an audience shared with other relying parties is
    // only ours if it says so.
    let (audience_includes_client, multiple_audiences) = match claims.get("aud") {
        Some(serde_json::Value::String(aud)) => (aud == &flow.client_id, false),
        Some(serde_json::Value::Array(list)) => (
            list.iter()
                .any(|aud| aud.as_str() == Some(flow.client_id.as_str())),
            list.len() > 1,
        ),
        _ => (false, false),
    };
    if !audience_includes_client {
        bail!(
            "id_token audience does not include client {}; aborting enrollment",
            flow.client_id
        );
    }
    match claims.get("azp") {
        Some(azp) if azp.as_str() != Some(flow.client_id.as_str()) => bail!(
            "id_token authorized party is not client {}; aborting enrollment",
            flow.client_id
        ),
        None if multiple_audiences => bail!(
            "id_token names several audiences but carries no authorized party (azp) for \
             client {}; aborting enrollment",
            flow.client_id
        ),
        _ => {}
    }

    // RFC 7519 NumericDate is a JSON number, not necessarily an integer: a
    // fractional `exp` is legal, and is floored to whole seconds here. A
    // non-numeric or negative value is not a NumericDate and is refused.
    let Some(exp) = claims.get("exp").and_then(serde_json::Value::as_f64) else {
        bail!("id_token carries no numeric exp claim; aborting enrollment");
    };
    if !exp.is_finite() || exp < 0.0 {
        bail!("id_token exp claim is not a valid NumericDate; aborting enrollment");
    }
    // Finite and non-negative, and a float-to-integer cast saturates rather
    // than wrapping, so a far-future exp stays far-future.
    let exp = exp.floor() as u64;
    if exp.saturating_add(ID_TOKEN_CLOCK_LEEWAY_SECS) <= now {
        bail!("id_token has expired; aborting enrollment");
    }

    match claims.get("nonce").and_then(|v| v.as_str()) {
        Some(nonce) if nonce == flow.nonce => Ok(()),
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
        let (discovery, issuer) = self.discovery().await?;
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
            issuer,
            client_id: self.config.effective_client_id().to_string(),
        })
    }

    /// Exchange the authorization code from the callback. When the
    /// response bundles an id_token it is validated against this flow
    /// (issuer, audience, expiry, nonce) before anything is returned; a
    /// present but malformed id_token fails the exchange.
    pub async fn pkce_exchange(&self, flow: &PkceFlow, code: &str) -> Result<EnrolledToken> {
        // The exchange posts this client's credentials to the token endpoint
        // pinned when the flow started. If the alias was repointed to another
        // issuer or client while the flow was pending, refuse before any
        // credential-bearing request: otherwise a live A->B provider swap
        // would send B's client secret to A's endpoint, and an eventual
        // invalid-code rejection could not undo that disclosure.
        if self.config.issuer != flow.issuer {
            bail!(
                "this sign-in was started for issuer {} but the alias now points at {}; \
                 refusing to send credentials to a changed provider",
                flow.issuer,
                self.config.issuer
            );
        }
        if self.config.effective_client_id() != flow.client_id {
            bail!(
                "this sign-in was started for client {} but the alias now uses {}; \
                 refusing to send credentials for a changed client",
                flow.client_id,
                self.config.effective_client_id()
            );
        }
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
        let status = response.status();
        let body = read_response_limited(response)
            .await
            .context("token response could not be read")?;
        if !status.is_success() {
            let err: OAuthError = serde_json::from_slice(&body)
                .with_context(|| format!("token endpoint returned HTTP {status}"))?;
            bail!(
                "authorization code exchange failed: {}{}",
                err.error,
                err.error_description
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            );
        }
        let raw: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&body).context("token response is not valid JSON")?;
        match raw.get("id_token") {
            None => {}
            Some(serde_json::Value::String(id_token)) => {
                validate_id_token(id_token, flow, now_unix())?;
            }
            Some(_) => bail!("token response carries a non-string id_token; refusing it"),
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
    IssuerMismatch(String),
    Ignore,
}

/// How long one connection has to deliver its request line. The deadline
/// covers the whole read rather than each `read` call, so a client that
/// dribbles bytes cannot extend its hold on the listener either.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How many connections may burn that deadline before the wait gives up.
/// Only a connection that goes silent holds the port; one that answers
/// promptly costs the wait nothing and must not be able to fail an
/// enrollment, because localhost port probes are ordinary background noise
/// on a developer machine. Past this many stalls the port is being held by
/// something else, and a named failure tells the operator more than a
/// silent wait until the flow deadline.
const MAX_STALLED_CALLBACK_CONNECTIONS: usize = 32;

/// The one-shot loopback callback listener (RFC 8252): binds an
/// ephemeral 127.0.0.1 port, answers exactly one matching callback, and
/// shuts down. Requests that do not carry this flow's state get a fixed
/// page and the wait continues; nothing from any request is echoed back.
/// Connections are served one at a time, so each gets a short read
/// deadline: a local process that connects and stays silent is dropped and
/// the next connection is taken, instead of parking the browser's callback
/// behind it until the whole flow times out.
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

    /// Wait for the callback carrying `flow`'s state. An IdP `error`
    /// response for the matching state fails the flow, as does a response
    /// whose `iss` names another issuer (RFC 9207); everything else (wrong
    /// path, wrong state, unparsable, or never sent) is answered or dropped
    /// and skipped. `timeout` is the outer bound on the whole wait; within
    /// it each connection gets `CALLBACK_READ_TIMEOUT`, and at most
    /// `MAX_STALLED_CALLBACK_CONNECTIONS` may expire it. A request answered
    /// promptly never counts against that bound.
    pub async fn wait_for_code(self, flow: &PkceFlow, timeout: Duration) -> Result<String> {
        match tokio::time::timeout(timeout, self.accept_loop(flow)).await {
            Ok(result) => result,
            Err(_) => bail!("timed out waiting for the browser sign-in to complete"),
        }
    }

    async fn accept_loop(&self, flow: &PkceFlow) -> Result<String> {
        let mut stalled = 0usize;
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            // A connection that does not complete a request line in time is
            // closed and costs nothing but its turn: it is not the callback,
            // so it does not consume the one-shot and the wait goes on.
            let target = tokio::time::timeout(
                CALLBACK_READ_TIMEOUT,
                Self::read_request_target(&mut stream),
            )
            .await;
            let target = match target {
                Ok(Ok(target)) => target,
                // Only a stall costs the wait real time, so only a stall is
                // counted. A client that says something and is answered
                // leaves the listener free for the browser however many of
                // them arrive, which is what a port probe does.
                Err(_) => {
                    stalled += 1;
                    if stalled >= MAX_STALLED_CALLBACK_CONNECTIONS {
                        bail!(
                            "the sign-in callback port was busy with other connections before the browser reached it"
                        );
                    }
                    continue;
                }
                Ok(Err(_)) => continue,
            };
            match Self::parse_callback(&target, flow) {
                CallbackParse::Code(code) => {
                    Self::respond(&mut stream, 200, SUCCESS_PAGE).await;
                    return Ok(code);
                }
                CallbackParse::IdpError(err) => {
                    Self::respond(&mut stream, 200, FAILURE_PAGE).await;
                    bail!("the identity provider denied the sign-in: {err}");
                }
                CallbackParse::IssuerMismatch(err) => {
                    Self::respond(&mut stream, 400, FAILURE_PAGE).await;
                    bail!("{err}");
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

    fn parse_callback(target: &str, flow: &PkceFlow) -> CallbackParse {
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
        let mut iss = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "state" => state = Some(value.into_owned()),
                "code" => code = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                "error_description" => error_description = Some(value.into_owned()),
                "iss" => iss = Some(value.into_owned()),
                _ => {}
            }
        }
        // State gates everything: an error without this flow's state is
        // some other request's business, not a verdict on this flow.
        if state.as_deref() != Some(flow.state.as_str()) {
            return CallbackParse::Ignore;
        }
        // The issuer check precedes both the error and the code: a mixed-up
        // response is not acted on either way.
        if let Err(e) = flow.check_callback_issuer(iss.as_deref()) {
            return CallbackParse::IssuerMismatch(e.to_string());
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
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            _ => "Not Found",
        };
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
                "issuer": issuer,
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

    async fn idp_with_pkce_endpoints() -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn pkce_exchange_refuses_a_repointed_client_or_issuer() {
        let server = idp_with_pkce_endpoints().await;
        // The token endpoint would happily mint a token; the exchange must
        // refuse before ever sending credentials there, so a live A->B
        // provider swap can never disclose B's client secret to A.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "should-never-be-minted",
                "expires_in": 3600,
            })))
            .expect(0)
            .mount(&server)
            .await;
        let started_by = Enrollment::new(config(&server.uri(), Some("secret-a"))).unwrap();
        let flow = started_by
            .pkce_start("http://127.0.0.1:1/cb")
            .await
            .unwrap();

        // Same issuer, but the alias now uses a different client.
        let mut swapped_client = config(&server.uri(), Some("secret-b"));
        swapped_client.client_id = "other-client".into();
        let err = Enrollment::new(swapped_client)
            .unwrap()
            .pkce_exchange(&flow, "code")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed client"), "{err}");

        // A different issuer entirely is refused the same way.
        let other_idp = idp_with_pkce_endpoints().await;
        let err = Enrollment::new(config(&other_idp.uri(), Some("secret-b")))
            .unwrap()
            .pkce_exchange(&flow, "code")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed provider"), "{err}");
    }

    /// RFC 8414 section 3.3: the identifier the discovery document asserts must
    /// be exactly the configured one — a trailing slash makes it a different
    /// issuer. The refusal has to land before any endpoint out of that document
    /// is used, for every flow, or a substituted document steers the whole
    /// enrollment; the `expect(0)` mocks are what pin that.
    #[tokio::test]
    async fn discovery_refuses_a_document_whose_issuer_differs() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": format!("{issuer}/"),
                "device_authorization_endpoint": format!("{issuer}/device"),
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        for endpoint in ["/device", "/token"] {
            Mock::given(method("POST"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "never-minted",
                })))
                .expect(0)
                .mount(&server)
                .await;
        }
        let enrollment = Enrollment::new(config(&issuer, Some("s3cret"))).unwrap();
        let errors = [
            enrollment.device_grant_start().await.unwrap_err(),
            enrollment.device_grant_poll("dev-x").await.unwrap_err(),
            enrollment.client_credentials().await.unwrap_err(),
            enrollment
                .pkce_start("http://127.0.0.1:1/cb")
                .await
                .unwrap_err(),
        ];
        for err in errors {
            let msg = err.to_string();
            assert!(msg.contains(&format!("asserts issuer {issuer}/")), "{msg}");
            assert!(msg.contains(&format!("configured for {issuer}")), "{msg}");
            assert!(msg.contains("trailing slash"), "{msg}");
        }
    }

    /// The right issuer identifier is not enough: a cleartext `token_endpoint`
    /// on a routable host would carry the client secret, the PKCE verifier and
    /// the device code in the clear. Every flow has to refuse before the
    /// request is built, which the `expect(0)` mocks pin.
    /// A redirect is the way around the transport check discovery just ran:
    /// the endpoint it approved answers 3xx and names somewhere else, and on
    /// 307 or 308 reqwest re-sends the credential-bearing body there. The
    /// verification sibling refuses redirects for the same reason, so the
    /// mocks here pin that the enrollment endpoints really were reached and
    /// that nothing followed them onward.
    #[tokio::test]
    async fn redirects_never_carry_enrollment_credentials_onward() {
        for status in [302u16, 307, 308] {
            let server = MockServer::start().await;
            let sink = MockServer::start().await;
            let issuer = server.uri();
            let elsewhere = format!("{}/capture", sink.uri());
            Mock::given(method("GET"))
                .and(path("/.well-known/openid-configuration"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "issuer": issuer,
                    "device_authorization_endpoint": format!("{issuer}/device"),
                    "token_endpoint": format!("{issuer}/token"),
                })))
                .mount(&server)
                .await;
            for endpoint in ["/device", "/token"] {
                Mock::given(method("POST"))
                    .and(path(endpoint))
                    .respond_with(
                        ResponseTemplate::new(status).insert_header("location", &*elsewhere),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
            }
            Mock::given(path("/capture"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "followed-the-redirect",
                })))
                .expect(0)
                .mount(&sink)
                .await;

            let enrollment = Enrollment::new(config(&issuer, Some("s3cret"))).unwrap();
            assert!(
                enrollment.device_grant_start().await.is_err(),
                "device start followed a {status}"
            );
            assert!(
                enrollment.client_credentials().await.is_err(),
                "client credentials followed a {status}"
            );
            assert!(
                sink.received_requests()
                    .await
                    .unwrap_or_default()
                    .is_empty(),
                "a {status} carried enrollment credentials to another origin"
            );
        }
    }

    #[tokio::test]
    async fn discovery_refuses_a_cleartext_token_endpoint() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "device_authorization_endpoint": format!("{issuer}/device"),
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": "http://203.0.113.5/token",
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        for endpoint in ["/device", "/token"] {
            Mock::given(method("POST"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "never-minted",
                })))
                .expect(0)
                .mount(&server)
                .await;
        }
        let enrollment = Enrollment::new(config(&issuer, Some("s3cret"))).unwrap();
        let errors = [
            enrollment.device_grant_start().await.unwrap_err(),
            enrollment.device_grant_poll("dev-x").await.unwrap_err(),
            enrollment.client_credentials().await.unwrap_err(),
            enrollment
                .pkce_start("http://127.0.0.1:1/cb")
                .await
                .unwrap_err(),
        ];
        for err in errors {
            let msg = err.to_string();
            assert!(msg.contains("token_endpoint"), "{msg}");
            assert!(msg.contains("https"), "{msg}");
        }
    }

    /// A cleartext `authorization_endpoint` would put the code challenge and
    /// the redirect into a browser over plain HTTP, so `pkce_start` refuses it
    /// even though the token endpoint beside it is fine.
    #[tokio::test]
    async fn pkce_start_refuses_a_cleartext_authorization_endpoint() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": "http://203.0.113.5/authorize",
                "token_endpoint": format!("{issuer}/token"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&issuer, Some("s3cret"))).unwrap();
        let err = enrollment
            .pkce_start("http://127.0.0.1:1/cb")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("authorization_endpoint"), "{msg}");
        assert!(msg.contains("https"), "{msg}");
    }

    /// Same for the device authorization endpoint: the start request carries
    /// the client identity and returns the device code, so a cleartext one is
    /// refused before `device_grant_start` posts anything.
    #[tokio::test]
    async fn device_grant_start_refuses_a_cleartext_device_endpoint() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "device_authorization_endpoint": "http://203.0.113.5/device",
                "token_endpoint": format!("{issuer}/token"),
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&issuer, Some("s3cret"))).unwrap();
        let err = enrollment.device_grant_start().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("device_authorization_endpoint"), "{msg}");
        assert!(msg.contains("https"), "{msg}");
    }

    #[tokio::test]
    async fn discovery_refuses_a_document_without_an_issuer() {
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
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&issuer, None)).unwrap();
        let err = enrollment.device_grant_start().await.unwrap_err();
        assert!(err.to_string().contains("no issuer"), "{err}");
    }

    /// An IdP document is untrusted input of unbounded length. The shared
    /// bounded reader refuses an oversized body before it is parsed, so no
    /// endpoint out of it is ever reached.
    #[tokio::test]
    async fn discovery_refuses_an_oversized_document() {
        use super::super::oidc::MAX_OIDC_RESPONSE_BYTES;
        let server = MockServer::start().await;
        let issuer = server.uri();
        let mut document = serde_json::json!({
            "issuer": issuer,
            "device_authorization_endpoint": format!("{issuer}/device"),
            "token_endpoint": format!("{issuer}/token"),
        });
        document["padding"] = serde_json::json!("x".repeat(MAX_OIDC_RESPONSE_BYTES));
        let body = document.to_string();
        assert!(
            body.len() > MAX_OIDC_RESPONSE_BYTES,
            "the fixture must exceed the cap"
        );
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let enrollment = Enrollment::new(config(&issuer, None)).unwrap();
        let err = enrollment.device_grant_start().await.unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("size limit"), "{chain}");
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
                "issuer": issuer,
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
    async fn device_grant_poll_surfaces_an_idp_denial() {
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
        match enrollment.device_grant_poll("dev-x").await.unwrap() {
            DevicePollOutcome::Denied(reason) => {
                assert!(reason.contains("access_denied"), "{reason}");
                assert!(reason.contains("user rejected"), "{reason}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
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

    /// The device code is the bearer credential of an in-flight grant, so it
    /// must never reach a log through a `{:?}`; the rest of the start response
    /// is shown to the user anyway and stays legible.
    #[test]
    fn device_grant_start_debug_redacts_secrets() {
        let start = DeviceGrantStart {
            device_code: "raw-device-code".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://sso.example.com/activate".into(),
            verification_uri_complete: Some("https://sso.example.com/activate?c=ABCD".into()),
            expires_in: 600,
            interval: 5,
        };
        let dbg = format!("{start:?}");
        assert!(!dbg.contains("raw-device-code"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
        assert!(dbg.contains("ABCD-EFGH"), "{dbg}");
        assert!(dbg.contains("sso.example.com/activate"), "{dbg}");
        assert!(dbg.contains("600"), "{dbg}");
    }
    async fn idp_with_pkce(methods: Option<serde_json::Value>) -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        let mut discovery = serde_json::json!({
            "issuer": issuer,
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

    fn flow_for(state: &str, issuer: &str) -> PkceFlow {
        PkceFlow {
            authorize_url: String::new(),
            state: state.into(),
            nonce: "flow-nonce".into(),
            verifier: "v".into(),
            redirect_uri: "http://127.0.0.1:1/callback".into(),
            token_endpoint: format!("{issuer}/token"),
            issuer: issuer.into(),
            client_id: "zerocode-cli".into(),
        }
    }

    /// A JWT-shaped id_token carrying `claims`. The signature segment is
    /// arbitrary: the direct token-endpoint exchange relies on TLS rather
    /// than a separate signature check (OIDC Core 3.1.3.7 step 6), so the
    /// claim checks are what these tests pin.
    fn id_token_with(claims: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{payload}.sig")
    }

    fn valid_claims(flow: &PkceFlow) -> serde_json::Value {
        serde_json::json!({
            "iss": flow.issuer,
            "aud": flow.client_id,
            "exp": now_unix() + 600,
            "nonce": flow.nonce,
        })
    }

    #[test]
    fn id_token_validation_accepts_a_token_for_this_flow() {
        let flow = flow_for("s", "https://idp.example.com");
        validate_id_token(&id_token_with(valid_claims(&flow)), &flow, now_unix()).unwrap();
        // An audience list naming this client is fine, with a matching azp.
        let mut claims = valid_claims(&flow);
        claims["aud"] = serde_json::json!(["other-app", flow.client_id]);
        claims["azp"] = serde_json::json!(flow.client_id);
        validate_id_token(&id_token_with(claims), &flow, now_unix()).unwrap();
        // A single-entry audience array names one party, so OIDC Core 3.1.3.7
        // step 4 asks for no azp; neither does a string audience (above).
        let mut claims = valid_claims(&flow);
        claims["aud"] = serde_json::json!([flow.client_id]);
        validate_id_token(&id_token_with(claims), &flow, now_unix()).unwrap();
        // RFC 7519 NumericDate may carry a fraction of a second; it is
        // floored to whole seconds rather than refused.
        let mut claims = valid_claims(&flow);
        claims["exp"] = serde_json::json!(now_unix() as f64 + 600.5);
        validate_id_token(&id_token_with(claims), &flow, now_unix()).unwrap();
        // Expiry is checked with a little clock leeway.
        let mut claims = valid_claims(&flow);
        claims["exp"] = serde_json::json!(now_unix() - ID_TOKEN_CLOCK_LEEWAY_SECS + 5);
        validate_id_token(&id_token_with(claims), &flow, now_unix()).unwrap();
    }

    #[test]
    fn id_token_validation_rejects_every_claim_defect() {
        let flow = flow_for("s", "https://idp.example.com");
        let mut cases: Vec<(&str, serde_json::Value)> = Vec::new();
        let mut c = valid_claims(&flow);
        c.as_object_mut().unwrap().remove("iss");
        cases.push(("no issuer", c));
        let mut c = valid_claims(&flow);
        c["iss"] = serde_json::json!("https://other-idp.example.com");
        cases.push(("issuer", c));
        let mut c = valid_claims(&flow);
        c["iss"] = serde_json::json!("https://idp.example.com/");
        cases.push(("issuer", c));
        let mut c = valid_claims(&flow);
        c["aud"] = serde_json::json!("someone-else");
        cases.push(("audience", c));
        let mut c = valid_claims(&flow);
        c["aud"] = serde_json::json!(["someone-else", "another"]);
        cases.push(("audience", c));
        let mut c = valid_claims(&flow);
        c.as_object_mut().unwrap().remove("aud");
        cases.push(("audience", c));
        let mut c = valid_claims(&flow);
        c["aud"] = serde_json::json!(["other-app", flow.client_id]);
        c["azp"] = serde_json::json!("other-app");
        cases.push(("authorized party", c));
        // More than one audience without an azp: OIDC Core 3.1.3.7 step 4
        // makes azp REQUIRED there, so the token is not demonstrably ours.
        let mut c = valid_claims(&flow);
        c["aud"] = serde_json::json!(["other-app", flow.client_id]);
        cases.push(("authorized party", c));
        let mut c = valid_claims(&flow);
        c["exp"] = serde_json::json!(now_unix() - ID_TOKEN_CLOCK_LEEWAY_SECS - 10);
        cases.push(("expired", c));
        let mut c = valid_claims(&flow);
        c.as_object_mut().unwrap().remove("exp");
        cases.push(("exp", c));
        // A NumericDate is a number: a string is not one, and neither is a
        // negative instant. A fractional one is floored, so an ancient
        // fraction is expired rather than accepted.
        let mut c = valid_claims(&flow);
        c["exp"] = serde_json::json!("1700000000");
        cases.push(("no numeric exp", c));
        let mut c = valid_claims(&flow);
        c["exp"] = serde_json::json!(-1.0);
        cases.push(("not a valid NumericDate", c));
        let mut c = valid_claims(&flow);
        c["exp"] = serde_json::json!(1.5);
        cases.push(("expired", c));
        let mut c = valid_claims(&flow);
        c["nonce"] = serde_json::json!("some-other-flows-nonce");
        cases.push(("nonce", c));
        let mut c = valid_claims(&flow);
        c.as_object_mut().unwrap().remove("nonce");
        cases.push(("nonce", c));

        for (expected, claims) in cases {
            let err =
                validate_id_token(&id_token_with(claims.clone()), &flow, now_unix()).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "claims {claims} should fail on {expected}, got: {err}"
            );
        }
    }

    #[test]
    fn id_token_validation_rejects_malformed_tokens() {
        let flow = flow_for("s", "https://idp.example.com");
        let payload = URL_SAFE_NO_PAD.encode(valid_claims(&flow).to_string());
        for (label, token) in [
            ("two segments", format!("hdr.{payload}")),
            ("four segments", format!("hdr.{payload}.sig.extra")),
            ("not base64url", "hdr.!!!.sig".to_string()),
            (
                "not json",
                format!("hdr.{}.sig", URL_SAFE_NO_PAD.encode("not json")),
            ),
            (
                "not an object",
                format!("hdr.{}.sig", URL_SAFE_NO_PAD.encode("[1,2]")),
            ),
        ] {
            assert!(
                validate_id_token(&token, &flow, now_unix()).is_err(),
                "{label} must be refused"
            );
        }
    }

    #[test]
    fn callback_issuer_check_follows_rfc_9207() {
        let flow = flow_for("s", "https://idp.example.com");
        flow.check_callback_issuer(None).unwrap();
        flow.check_callback_issuer(Some("https://idp.example.com"))
            .unwrap();
        let err = flow
            .check_callback_issuer(Some("https://different.example"))
            .unwrap_err();
        assert!(err.to_string().contains("mix-up"), "{err}");
        // Exact comparison: a trailing slash is a different issuer string.
        assert!(
            flow.check_callback_issuer(Some("https://idp.example.com/"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn pkce_exchange_sends_the_verifier_and_validates_the_id_token() {
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
                "id_token": id_token_with(valid_claims(&flow)),
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
        let mut claims = valid_claims(&flow);
        claims["nonce"] = serde_json::json!("some-other-flows-nonce");
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-substituted",
                "id_token": id_token_with(claims),
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
    async fn pkce_exchange_rejects_id_tokens_for_another_issuer_or_audience() {
        for (label, mutate) in [
            (
                "issuer",
                serde_json::json!({"iss": "https://other-idp.example.com"}),
            ),
            ("audience", serde_json::json!({"aud": "someone-else"})),
            ("expired", serde_json::json!({"exp": 1})),
        ] {
            let server = idp_with_pkce(Some(serde_json::json!(["S256"]))).await;
            let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
            let flow = enrollment
                .pkce_start("http://127.0.0.1:7777/callback")
                .await
                .unwrap();
            let mut claims = valid_claims(&flow);
            for (k, v) in mutate.as_object().unwrap() {
                claims[k] = v.clone();
            }
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "at-never-returned",
                    "id_token": id_token_with(claims),
                })))
                .mount(&server)
                .await;
            let err = enrollment
                .pkce_exchange(&flow, "auth-code-1")
                .await
                .unwrap_err();
            assert!(err.to_string().contains(label), "{label}: {err}");
        }
    }

    #[tokio::test]
    async fn pkce_exchange_rejects_a_present_non_string_or_malformed_id_token() {
        for (label, id_token) in [
            ("non-string", serde_json::json!(42)),
            ("object", serde_json::json!({"nonce": "x"})),
            ("malformed", serde_json::json!("not.a.jwt.at.all")),
        ] {
            let server = idp_with_pkce(Some(serde_json::json!(["S256"]))).await;
            let enrollment = Enrollment::new(config(&server.uri(), None)).unwrap();
            let flow = enrollment
                .pkce_start("http://127.0.0.1:7777/callback")
                .await
                .unwrap();
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "at-never-returned",
                    "id_token": id_token,
                })))
                .mount(&server)
                .await;
            let err = enrollment
                .pkce_exchange(&flow, "auth-code-1")
                .await
                .unwrap_err();
            assert!(err.to_string().contains("id_token"), "{label}: {err}");
        }
    }

    #[tokio::test]
    async fn loopback_listener_ignores_wrong_state_and_returns_the_matching_code() {
        let listener = LoopbackListener::bind().await.unwrap();
        let base = listener.redirect_uri();
        let flow = flow_for("good-state", "https://idp.example.com");
        let wait = listener.wait_for_code(&flow, Duration::from_secs(5));
        let drive = async {
            // Wrong state: answered, ignored, the wait continues.
            let resp = reqwest::get(format!("{base}?code=evil&state=bad-state"))
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 404);
            // Matching state and a matching RFC 9207 issuer: the code comes
            // back and the listener is done.
            let resp = reqwest::get(format!(
                "{base}?code=real-code&state=good-state&iss=https%3A%2F%2Fidp.example.com"
            ))
            .await
            .unwrap();
            assert_eq!(resp.status().as_u16(), 200);
        };
        let (code, ()) = tokio::join!(wait, drive);
        assert_eq!(code.unwrap(), "real-code");
    }

    /// A raw callback request, so a test can queue one without a client that
    /// keeps its own connection pool and timers.
    fn callback_request(query: &str) -> Vec<u8> {
        format!("GET /callback?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").into_bytes()
    }

    #[tokio::test(start_paused = true)]
    async fn loopback_listener_skips_a_connection_that_never_sends_a_request() {
        use tokio::io::AsyncWriteExt as _;

        // The stalled connections are queued before the listener starts
        // accepting, so they are served first: each must be dropped at the
        // read deadline instead of holding the browser's callback behind it
        // for the whole flow timeout. One says nothing at all, the other
        // dribbles a partial request line and stops. The clock is paused, so
        // the skips cost the test no wall time.
        let listener = LoopbackListener::bind().await.unwrap();
        let addr = format!("127.0.0.1:{}", listener.port);
        let flow = flow_for("good-state", "https://idp.example.com");
        let _silent = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let mut partial = tokio::net::TcpStream::connect(&addr).await.unwrap();
        partial.write_all(b"GET /callb").await.unwrap();
        let mut browser = tokio::net::TcpStream::connect(&addr).await.unwrap();
        browser
            .write_all(&callback_request(
                "code=real-code&state=good-state&iss=https%3A%2F%2Fidp.example.com",
            ))
            .await
            .unwrap();

        let code = listener
            .wait_for_code(&flow, Duration::from_secs(300))
            .await
            .unwrap();
        assert_eq!(code, "real-code");
    }

    #[tokio::test(start_paused = true)]
    async fn loopback_listener_gives_up_after_too_many_silent_connections() {
        // The skip is bounded: a local process that keeps the port busy ends
        // the wait with a named failure rather than an unexplained one at the
        // flow deadline.
        let listener = LoopbackListener::bind().await.unwrap();
        let addr = format!("127.0.0.1:{}", listener.port);
        let flow = flow_for("good-state", "https://idp.example.com");
        let mut held = Vec::new();
        for _ in 0..MAX_STALLED_CALLBACK_CONNECTIONS {
            held.push(tokio::net::TcpStream::connect(&addr).await.unwrap());
        }

        let err = listener
            .wait_for_code(&flow, Duration::from_secs(300))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("callback port was busy"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn loopback_listener_serves_the_callback_after_many_ignored_requests() {
        use tokio::io::AsyncWriteExt as _;

        // A request that is answered promptly never holds the port, so any
        // number of them may arrive before the browser does: a localhost
        // port probe is ordinary background noise and must not be able to
        // fail an enrollment. Every request is queued in full before the
        // wait starts, so no read blocks and the clock never advances.
        let listener = LoopbackListener::bind().await.unwrap();
        let addr = format!("127.0.0.1:{}", listener.port);
        let flow = flow_for("good-state", "https://idp.example.com");
        let mut probes = Vec::new();
        for _ in 0..MAX_STALLED_CALLBACK_CONNECTIONS + 8 {
            let mut probe = tokio::net::TcpStream::connect(&addr).await.unwrap();
            probe
                .write_all(&callback_request("code=evil&state=bad-state"))
                .await
                .unwrap();
            probes.push(probe);
        }
        let mut browser = tokio::net::TcpStream::connect(&addr).await.unwrap();
        browser
            .write_all(&callback_request(
                "code=real-code&state=good-state&iss=https%3A%2F%2Fidp.example.com",
            ))
            .await
            .unwrap();

        let code = listener
            .wait_for_code(&flow, Duration::from_secs(300))
            .await
            .unwrap();
        assert_eq!(code, "real-code");
    }

    #[tokio::test]
    async fn loopback_listener_fails_the_flow_on_an_idp_error() {
        let listener = LoopbackListener::bind().await.unwrap();
        let base = listener.redirect_uri();
        let flow = flow_for("good-state", "https://idp.example.com");
        let wait = listener.wait_for_code(&flow, Duration::from_secs(5));
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

    #[tokio::test]
    async fn loopback_listener_refuses_a_callback_from_another_issuer() {
        // A matching state with a foreign `iss` is a mix-up (RFC 9207): the
        // flow fails before any code is returned, whether the response
        // carries a code or an IdP error.
        for query in [
            "code=real-code&state=good-state&iss=https%3A%2F%2Fdifferent.example",
            "error=access_denied&state=good-state&iss=https%3A%2F%2Fdifferent.example",
        ] {
            let listener = LoopbackListener::bind().await.unwrap();
            let base = listener.redirect_uri();
            let flow = flow_for("good-state", "https://idp.example.com");
            let wait = listener.wait_for_code(&flow, Duration::from_secs(5));
            let drive = async {
                let resp = reqwest::get(format!("{base}?{query}")).await.unwrap();
                assert_eq!(resp.status().as_u16(), 400);
                let page = resp.text().await.unwrap();
                assert!(
                    !page.contains("different.example"),
                    "no echo of the request"
                );
            };
            let (result, ()) = tokio::join!(wait, drive);
            let err = result.unwrap_err();
            assert!(err.to_string().contains("mix-up"), "{err}");
        }
    }
}
