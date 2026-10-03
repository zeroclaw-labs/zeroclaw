//! Bot Framework authentication for the Microsoft Teams channel.
//!
//! Two directions, two types:
//!
//! - **Inbound** — every activity POST from the Bot Connector service
//!   carries a `Authorization: Bearer <JWT>` header. [`JwtValidator`]
//!   verifies the RS256 signature against the issuer's JWKS document
//!   (discovered through OpenID metadata), plus issuer (the Bot Framework
//!   issuer only), audience (= the bot's `app_id`), expiry and
//!   validity-start, before any payload is trusted. It also surfaces the
//!   signing key's channel `endorsements` and the signed `serviceurl`
//!   claim so the listener can bind them to the activity body.
//! - **Outbound** — Connector API calls authenticate with an Entra
//!   client-credentials token. [`ConnectorTokenProvider`] fetches one and
//!   caches it until shortly before expiry, or until the credentials it was
//!   minted for stop matching the ones the caller passes.
//!
//! This is the only msteams module that touches key material or tokens.
//! Credentials are passed in per call (resolved from canonical `Config`
//! by the caller); neither type stores `app_id` / `app_password`.

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Issuer of Bot Connector service tokens for multi-tenant bots.
pub const BOT_FRAMEWORK_ISSUER: &str = "https://api.botframework.com";

/// OpenID metadata document for Bot Connector service tokens. Its
/// `jwks_uri` field points at the signing keys.
pub const BOT_FRAMEWORK_OPENID_METADATA_URL: &str =
    "https://login.botframework.com/v1/.well-known/openidconfiguration";

/// OAuth scope for Connector API client-credentials tokens.
pub const CONNECTOR_TOKEN_SCOPE: &str = "https://api.botframework.com/.default";

/// Clock-skew tolerance (seconds) for `exp`/`nbf` checks, per the Bot
/// Framework authentication spec ("allow for up to 5 minutes").
const JWT_CLOCK_SKEW_LEEWAY_SECS: u64 = 300;

/// Minimum spacing between JWKS refresh *attempts*, successful or not.
/// Inbound tokens name their `kid` before any signature is verified, so an
/// unauthenticated flood of unknown key ids must not translate into one
/// outbound fetch per request, including while the issuer is failing.
const JWKS_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Force a JWKS re-fetch once the cache reaches this age, even for a
/// `kid` that is still cached. Microsoft asks callers to refresh the Bot
/// Framework keys at least daily; without this bound a key the issuer has
/// *withdrawn* would stay trusted until the next unknown-`kid` miss (or a
/// process restart), leaving a rotated-out key usable indefinitely.
const JWKS_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Refresh the cached connector token this long before it expires, so an
/// outbound send never races token expiry mid-request.
const CONNECTOR_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// Entra token endpoint (client-credentials flow) for a tenant.
#[must_use]
pub fn connector_token_url(tenant_id: &str) -> String {
    format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token")
}

/// Issuers accepted on inbound service tokens. Connector-to-bot tokens
/// are always minted by the Bot Framework issuer
/// ([`BOT_FRAMEWORK_ISSUER`]); the tenant's Entra issuers mint tokens for
/// *outbound* Graph/SSO flows, not for the activity POSTs this listener
/// authenticates, so accepting them here would widen the trust boundary
/// with no legitimate caller. The set is a single entry, kept as a `Vec`
/// for `jsonwebtoken`'s `set_issuer` API.
#[must_use]
pub fn connector_issuers() -> Vec<String> {
    vec![BOT_FRAMEWORK_ISSUER.to_string()]
}

/// Extract the token from an `Authorization` header value. Returns `None`
/// unless the scheme is exactly `Bearer` with a non-empty token.
#[must_use]
pub fn bearer_token(header_value: &str) -> Option<&str> {
    let token = header_value.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then_some(token)
}

/// Why an inbound token or outbound token exchange was rejected.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("malformed JWT: {0}")]
    MalformedToken(#[source] jsonwebtoken::errors::Error),
    #[error("unsupported JWT algorithm {0:?}; only RS256 is accepted")]
    UnsupportedAlgorithm(Algorithm),
    #[error("JWT header carries no key id (kid)")]
    MissingKeyId,
    #[error("JWT signed with unknown key id {0:?}")]
    UnknownKeyId(String),
    #[error("JWKS fetch failed: {0}")]
    JwksFetch(String),
    /// The cached keys cannot be trusted (absent, or older than
    /// [`JWKS_MAX_AGE`]) and the refresh rate limit forbids a fetch right
    /// now. Distinct from [`AuthError::UnknownKeyId`]: the key set could
    /// not be consulted at all, so this signals a broken issuer or a token
    /// flood rather than a token that names a retired key.
    #[error("no trusted JWKS available: cache is stale or empty and refresh is rate-limited")]
    KeysUnavailable,
    #[error("JWKS entry for key id {kid:?} is not a usable RSA key: {reason}")]
    UnusableJwk { kid: String, reason: String },
    #[error("JWT rejected: {0}")]
    Rejected(#[source] jsonwebtoken::errors::Error),
    #[error("token endpoint returned HTTP {status}: {body}")]
    TokenEndpoint { status: u16, body: String },
    #[error("token endpoint request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// Everything a validated inbound service token surfaces for the
/// listener's downstream binding checks. Issuer, audience, expiry and
/// validity-start are enforced during validation and not re-exposed.
///
/// - `serviceurl` is the *signed* Connector base URL; the listener must
///   confirm it matches the activity's `serviceUrl` before recording any
///   conversation reference or attaching the bot's Connector token to an
///   outbound request there.
/// - `endorsements` are the channel ids the *signing key* is published to
///   sign for; the listener confirms the activity's `channelId` is among
///   them, per Microsoft's Bot Connector authentication contract.
#[derive(Debug)]
pub struct ValidatedClaims {
    pub serviceurl: Option<String>,
    pub endorsements: Vec<String>,
}

/// Registered claims decoded from a service token. Only `serviceurl` is
/// surfaced to the caller; the rest are enforced by `jsonwebtoken` via
/// [`Validation`] and never re-read here.
#[derive(Deserialize)]
struct ServiceTokenClaims {
    #[serde(default)]
    serviceurl: Option<String>,
}

#[derive(Deserialize)]
struct OpenIdMetadata {
    jwks_uri: String,
}

#[derive(Deserialize)]
struct JwksDocument {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    kty: String,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    /// Channel ids this key is authorized to sign for. Bot Framework
    /// keys publish this; an activity's `channelId` must appear here.
    #[serde(default)]
    endorsements: Vec<String>,
}

/// A usable RSA signing key from the issuer's JWKS, with the channel
/// endorsements published alongside it.
#[derive(Clone)]
struct JwkKey {
    /// RSA modulus `n` (base64url).
    n: String,
    /// RSA public exponent `e` (base64url).
    e: String,
    /// Channel ids this key may sign for (see [`Jwk::endorsements`]).
    endorsements: Vec<String>,
}

#[derive(Default)]
struct JwksCache {
    /// `kid` -> RSA key + endorsements, as served by the issuer's JWKS
    /// document. Materialized view of the issuer's keys; refreshed on
    /// unknown-`kid` misses and once the cache passes [`JWKS_MAX_AGE`],
    /// never edited locally.
    keys: HashMap<String, JwkKey>,
    /// When [`JwksCache::keys`] was last replaced by a successful fetch.
    /// Decides whether the cache may still serve a key at all
    /// ([`JWKS_MAX_AGE`]), so a failing issuer can never extend the life of
    /// a key set.
    last_fetch: Option<Instant>,
    /// When a fetch was last *attempted*, regardless of outcome. Decides
    /// whether another attempt is allowed
    /// ([`JWKS_REFRESH_MIN_INTERVAL`]). Kept separate from `last_fetch`
    /// because throttling on success alone leaves every request free to
    /// re-probe an issuer that is down.
    last_attempt: Option<Instant>,
}

/// What a call to [`JwtValidator::refresh_jwks`] actually did.
#[derive(Debug, PartialEq, Eq)]
enum RefreshOutcome {
    /// The issuer answered and [`JwksCache::keys`] now holds its current
    /// key set.
    Refreshed,
    /// The refresh interval had not elapsed, so no request was made and the
    /// cache is untouched.
    Throttled,
}

/// Supplies the HTTP client for an auth-side request.
///
/// Both auth egresses are resolved through one of these rather than
/// holding a client, because the proxy that has to carry them lives in
/// live config: a handle built once at startup would keep dialing direct
/// after a reload changed it. The channel installs a resolver that reads
/// the same per-channel `proxy_url` its Connector sends use.
pub type HttpClientResolver = Arc<dyn Fn() -> reqwest::Client + Send + Sync>;

/// The client used when no resolver is installed.
///
/// This still goes through the runtime proxy, so a caller that forgets to
/// install a resolver loses the per-channel `proxy_url` override but keeps
/// the global `[proxy]` settings the rest of the daemon obeys. A bare
/// `Client::builder()` here would honor only the environment variables, and
/// silently dialing Microsoft direct is the failure this resolver exists to
/// prevent — it presents as inbound activities rejected with 401 while the
/// proxy looks correctly configured.
fn default_auth_client() -> reqwest::Client {
    zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts("channel.msteams", 10, 10)
}

/// Validates inbound Bot Connector service tokens against the issuer's
/// published JWKS.
pub struct JwtValidator {
    http: HttpClientResolver,
    openid_metadata_url: String,
    jwks: tokio::sync::RwLock<JwksCache>,
    refresh_min_interval: Duration,
    max_age: Duration,
}

impl JwtValidator {
    /// `openid_metadata_url` is the OpenID configuration document whose
    /// `jwks_uri` serves the signing keys — normally
    /// [`BOT_FRAMEWORK_OPENID_METADATA_URL`].
    #[must_use]
    pub fn new(openid_metadata_url: impl Into<String>) -> Self {
        Self {
            http: Arc::new(default_auth_client),
            openid_metadata_url: openid_metadata_url.into(),
            jwks: tokio::sync::RwLock::new(JwksCache::default()),
            refresh_min_interval: JWKS_REFRESH_MIN_INTERVAL,
            max_age: JWKS_MAX_AGE,
        }
    }

    /// Route JWKS fetches through the caller's client, so they honor the
    /// channel's proxy instead of dialing Microsoft directly.
    #[must_use]
    pub fn with_http_client_resolver(mut self, http: HttpClientResolver) -> Self {
        self.http = http;
        self
    }

    /// Test hook: allow immediate JWKS re-fetches so key-rotation paths
    /// can be exercised without waiting out the production rate limit.
    #[cfg(test)]
    fn with_refresh_min_interval(mut self, interval: Duration) -> Self {
        self.refresh_min_interval = interval;
        self
    }

    /// Test hook: shrink the max cache age so the daily-refresh path can
    /// be exercised without waiting out the production 24h bound.
    #[cfg(test)]
    fn with_max_age(mut self, max_age: Duration) -> Self {
        self.max_age = max_age;
        self
    }

    /// Validate a bearer token (without the `Bearer ` prefix).
    ///
    /// `app_id` is the expected audience; `issuers` the accepted issuer
    /// set (see [`connector_issuers`]). `app_id` is resolved from
    /// canonical config by the caller at call time.
    pub async fn validate(
        &self,
        token: &str,
        app_id: &str,
        issuers: &[String],
    ) -> Result<ValidatedClaims, AuthError> {
        let header = jsonwebtoken::decode_header(token).map_err(AuthError::MalformedToken)?;
        if header.alg != Algorithm::RS256 {
            return Err(AuthError::UnsupportedAlgorithm(header.alg));
        }
        let kid = header.kid.ok_or(AuthError::MissingKeyId)?;
        let (key, endorsements) = self.decoding_key(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = JWT_CLOCK_SKEW_LEEWAY_SECS;
        // Reject tokens whose validity window has not opened yet; the
        // leeway above applies to `nbf` as well as `exp`.
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        validation.set_audience(&[app_id]);
        validation.set_issuer(issuers);

        let claims = jsonwebtoken::decode::<ServiceTokenClaims>(token, &key, &validation)
            .map(|data| data.claims)
            .map_err(AuthError::Rejected)?;
        Ok(ValidatedClaims {
            serviceurl: claims.serviceurl,
            endorsements,
        })
    }

    /// Resolve `kid` to its RSA key and channel endorsements.
    ///
    /// Every path that serves a key without having fetched it is guarded
    /// by [`JWKS_MAX_AGE`], so a key set the issuer may have changed can
    /// never be used past that age, whether the refresh failed or was
    /// rate-limited.
    async fn decoding_key(&self, kid: &str) -> Result<(DecodingKey, Vec<String>), AuthError> {
        if self.cache_within_max_age().await
            && let Some(key) = self.cached_key(kid).await?
        {
            return Ok(key);
        }
        // A throttled call issued no request of its own, but a concurrent
        // caller may have completed a refresh while this one queued on the
        // write lock, so the cache is consulted again rather than rejected
        // outright. It may only be trusted while the age bound still
        // vouches for it; a refreshed call just replaced the key set and
        // reads it directly.
        if self.refresh_jwks().await? == RefreshOutcome::Throttled
            && !self.cache_within_max_age().await
        {
            return Err(AuthError::KeysUnavailable);
        }
        match self.cached_key(kid).await? {
            Some(key) => Ok(key),
            None => Err(AuthError::UnknownKeyId(kid.to_string())),
        }
    }

    /// Whether the cached JWKS is younger than [`JwtValidator::max_age`]
    /// (and thus usable without a refresh). An empty cache is never fresh.
    async fn cache_within_max_age(&self) -> bool {
        let cache = self.jwks.read().await;
        cache
            .last_fetch
            .is_some_and(|last| last.elapsed() < self.max_age)
    }

    async fn cached_key(&self, kid: &str) -> Result<Option<(DecodingKey, Vec<String>)>, AuthError> {
        let cache = self.jwks.read().await;
        let Some(jwk) = cache.keys.get(kid) else {
            return Ok(None);
        };
        DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
            .map(|key| Some((key, jwk.endorsements.clone())))
            .map_err(|err| AuthError::UnusableJwk {
                kid: kid.to_string(),
                reason: err.to_string(),
            })
    }

    async fn refresh_jwks(&self) -> Result<RefreshOutcome, AuthError> {
        let mut cache = self.jwks.write().await;
        if cache
            .last_attempt
            .is_some_and(|last| last.elapsed() < self.refresh_min_interval)
        {
            return Ok(RefreshOutcome::Throttled);
        }
        // Stamped before the requests and kept even if they fail, so a
        // failing issuer is probed at most once per interval. Callers that
        // queued on this write lock see the closed window and do not pile
        // on a second attempt.
        cache.last_attempt = Some(Instant::now());

        // One client for both legs of the refresh: resolved here rather
        // than held, so the proxy in force is the one config names now.
        let http = (self.http)();
        let metadata: OpenIdMetadata = http
            .get(&self.openid_metadata_url)
            .send()
            .await
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?
            .error_for_status()
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?
            .json()
            .await
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?;

        let jwks: JwksDocument = http
            .get(&metadata.jwks_uri)
            .send()
            .await
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?
            .error_for_status()
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?
            .json()
            .await
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?;

        cache.keys = jwks
            .keys
            .into_iter()
            .filter(|key| key.kty == "RSA")
            .filter_map(|key| match (key.kid, key.n, key.e) {
                (Some(kid), Some(n), Some(e)) => Some((
                    kid,
                    JwkKey {
                        n,
                        e,
                        endorsements: key.endorsements,
                    },
                )),
                _ => None,
            })
            .collect();
        cache.last_fetch = Some(Instant::now());
        Ok(RefreshOutcome::Refreshed)
    }
}

impl std::fmt::Debug for JwtValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtValidator")
            .field("openid_metadata_url", &self.openid_metadata_url)
            .field("jwks", &"<cached keys>")
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
    /// Fingerprint of the credential pair Entra minted this token for.
    /// Source of truth created here: the credentials live in config and are
    /// passed per call, but nothing else records which of them produced the
    /// cached token, and a token outlives a rotation by up to its whole
    /// lifetime. Hashed rather than kept, so the cache can answer "not the
    /// same credentials" without holding the secret a second time.
    minted_for: [u8; 32],
}

/// Identify a credential pair without retaining it. The length prefix keeps
/// `("ab", "c")` and `("a", "bc")` apart, which a plain concatenation would
/// merge into one fingerprint.
fn credential_fingerprint(app_id: &str, app_password: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((app_id.len() as u64).to_le_bytes());
    hasher.update(app_id.as_bytes());
    hasher.update(app_password.as_bytes());
    hasher.finalize().into()
}

/// Fetches and caches the Entra client-credentials token used against the
/// Bot Connector API. The cached token is a time-bounded materialized
/// credential minted by Entra at runtime — the source of truth for the
/// *credentials* stays in config and is passed in per call.
pub struct ConnectorTokenProvider {
    http: HttpClientResolver,
    token_url: String,
    cached: tokio::sync::RwLock<Option<CachedToken>>,
}

impl ConnectorTokenProvider {
    #[must_use]
    pub fn new(token_url: impl Into<String>) -> Self {
        Self {
            http: Arc::new(default_auth_client),
            token_url: token_url.into(),
            cached: tokio::sync::RwLock::new(None),
        }
    }

    /// Route token requests through the caller's client, so they honor
    /// the channel's proxy instead of dialing Entra directly.
    #[must_use]
    pub fn with_http_client_resolver(mut self, http: HttpClientResolver) -> Self {
        self.http = http;
        self
    }

    /// Provider for a tenant's production Entra token endpoint.
    #[must_use]
    pub fn for_tenant(tenant_id: &str) -> Self {
        Self::new(connector_token_url(tenant_id))
    }

    /// Return a bearer token for the Connector API, fetching a fresh one when
    /// none is cached, the cached one is inside the refresh margin, or it was
    /// minted for different credentials than the caller now passes.
    pub async fn token(&self, app_id: &str, app_password: &str) -> Result<String, AuthError> {
        let fingerprint = credential_fingerprint(app_id, app_password);
        if let Some(token) = self.fresh_cached_token(&fingerprint).await {
            return Ok(token);
        }

        let mut cached = self.cached.write().await;
        // Another task may have refreshed while this one waited on the lock.
        if let Some(token) = cached.as_ref().filter(|t| Self::is_usable(t, &fingerprint)) {
            return Ok(token.access_token.clone());
        }

        let response = (self.http)()
            .post(&self.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", app_id),
                ("client_secret", app_password),
                ("scope", CONNECTOR_TOKEN_SCOPE),
            ])
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(AuthError::TokenEndpoint {
                status: status.as_u16(),
                body,
            });
        }

        let token: TokenResponse = response.json().await?;
        let access_token = token.access_token.clone();
        *cached = Some(CachedToken {
            access_token: token.access_token,
            expires_at: Instant::now() + Duration::from_secs(token.expires_in),
            minted_for: fingerprint,
        });
        Ok(access_token)
    }

    async fn fresh_cached_token(&self, fingerprint: &[u8; 32]) -> Option<String> {
        let cached = self.cached.read().await;
        cached
            .as_ref()
            .filter(|t| Self::is_usable(t, fingerprint))
            .map(|t| t.access_token.clone())
    }

    fn is_usable(token: &CachedToken, fingerprint: &[u8; 32]) -> bool {
        token.minted_for == *fingerprint && Self::is_fresh(token)
    }

    fn is_fresh(token: &CachedToken) -> bool {
        token.expires_at > Instant::now() + CONNECTOR_TOKEN_REFRESH_MARGIN
    }
}

impl std::fmt::Debug for ConnectorTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorTokenProvider")
            .field("token_url", &self.token_url)
            .field("cached", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Throwaway 2048-bit RSA key generated for unit tests only, never
/// registered with any Entra app or bot. Mirrors the fixture pattern in
/// `git/providers/github/auth.rs`.
#[cfg(test)]
pub(crate) const TEST_KEY_PEM: &str = concat!(
    "-----BEGIN ",
    "PRIVATE KEY-----\n",
    "MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDFfZQatRSisjPq
6w86nXPJ2w0zIv1TL3fBZLAuklRjoeRkVbXvAaoZWxkHZKnqhS0RI4od0KlJh66P
KZ+P4QdW4KVK2Rd3MrHYdxFrhHjJPzuobkFQjG3enp9Z3/t7Fh4fhg4p/4vBTAlT
hM8zFT56mFBj0J9NUHVtBkbdOYiKpISBHs0cIRrBpk4LZ755RfA23bsgLfp1i6fY
1fz003s7dGv+gCxApYh/ca1I120Ut0pJrgsN5e7+yXS6FHvMzK+dy5hbudOyjgyY
uixqveoIMqCwD4yMpXJAMSrRBvbZYgyNWYXrw1JqHoiKVeaOggsbY4IOfAYyZYWW
XoT1Zi4BAgMBAAECggEAEhQJDPnTEgKkmIZcfkXgDdQYzPvQuz7u5DvymczI31b4
SIwFC8Q3/Tq225OmL0LyKW26rLiHaqT6QH4zrlDP4m7NisC9MmroV1Os+03k3a1F
aYlwPq6gNx8HoMtNYsrXRpT3snYDZdYvS18utXMmJURQpZZ5IrNxEGIg9grYejJk
QihVlrRCcHIdrriCRLHfPgkJ3gJwhyrECCVt9UtS3rdwiKnTb/KsVyL2c+xLNdgF
zQPE8H2EO1IJ06iVFfj/lYGUYg618wIMy3s790fz1nBTNQmuo1S2ydjUQmTUOPWZ
2TkpqYhL7GjVxJZKhEuQd7FA50Ck7kMMuu8IWq/KMQKBgQD8h8DAuGDK5T8cJHiv
OuSHbeNQ0rtGcrZFRA5ygOQSqJGYdqqMmhS6fx2qLTv7Q1DDuI+COqySJyAUh8nX
XCTvRk0Mm65gHJAbnSmLgfQ4U4s+yvSo0PGdfrh4xKVMWdSuwpKr7ErmEbDveEpY
akt4UCwKDNQkH06c6DPPEcFnWQKBgQDINDq8y0esDkNS2hIauXOD/g+S9Mtn7tm6
qPGRe/THNTfsxwF0Q0uRVynWdNsYSUcMgz91N4bIEs3dXcn0WgkYK9V5NslHEdY4
3MEo1yAJMBuPtY1Drj7qVaJhqoflKPI9klI73lZct/sk4MGnUHMtgvPvrgm44PYd
pc11u7JO6QKBgQC/kmiaiwT6xsCCo/Rd0oqNZsKcjND/V4SItWFUYg0jTnftNpCZ
S0ZQWKBzeg9XxLBfWgKcY9CIq1+902k+lCt8zVMkLnIxfVmhaS+cIsDXfiFTSHok
GyZAOWLOUzem3TroPLkx7XbAZEla0WFtA24vXnqaQTMqGAn2JH0xKCIVOQKBgQCS
3FV2Jrxlx3S1c0iyl/XYDmfISpBnpnvLhKDoMwDlnPFwXK+BZNgrPsBvE/ugfiiD
UkgbqWbSn5CqYWGDQQTI2WbYa0sNOlVmEvITDnPuqX6eVfTRgCGg7r6WXG0hun6w
kgSG7Ft32nJ9o+4K2WYULarZ2FZDa6q/JuBoDA8J+QKBgCQNCzMebbicBiZx55IJ
TUK2QBWQREAMUKAI6jyZA+9YL7IKrAnbdjXVpE+zgRANOz6z1F2VGRTlo54Zvuym
02jyMNrkFaCKMO46BTBy6DEd9sIZ2W5ebUWsxWUx2SbMZihhsKDnVICQ4adompMm
5XuhfeBOhQKvW2zJOCmbbe+i
",
    "-----END ",
    "PRIVATE KEY-----\n",
);

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header};
    use serde::Serialize;
    use std::sync::Arc;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Base64url RSA modulus of [`TEST_KEY_PEM`]'s public half; exponent
    /// is the standard 65537 (`AQAB`).
    const TEST_KEY_N: &str = "xX2UGrUUorIz6usPOp1zydsNMyL9Uy93wWSwLpJUY6HkZFW17wGqGVsZB2Sp6oUt\
                              ESOKHdCpSYeujymfj-EHVuClStkXdzKx2HcRa4R4yT87qG5BUIxt3p6fWd_7exYe\
                              H4YOKf-LwUwJU4TPMxU-ephQY9CfTVB1bQZG3TmIiqSEgR7NHCEawaZOC2e-eUXw\
                              Nt27IC36dYun2NX89NN7O3Rr_oAsQKWIf3GtSNdtFLdKSa4LDeXu_sl0uhR7zMyv\
                              ncuYW7nTso4MmLosar3qCDKgsA-MjKVyQDEq0Qb22WIMjVmF68NSah6IilXmjoIL\
                              G2OCDnwGMmWFll6E9WYuAQ";
    const TEST_KID: &str = "test-signing-key-1";
    const APP_ID: &str = "00000000-aaaa-bbbb-cccc-000000000000";

    #[derive(Serialize)]
    struct TestClaims {
        iss: String,
        aud: String,
        exp: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        nbf: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        serviceurl: Option<String>,
    }

    fn mint(iss: &str, aud: &str, exp: i64, kid: Option<&str>) -> String {
        mint_with_nbf(iss, aud, exp, None, kid)
    }

    fn mint_with_nbf(
        iss: &str,
        aud: &str,
        exp: i64,
        nbf: Option<i64>,
        kid: Option<&str>,
    ) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_string);
        let claims = TestClaims {
            iss: iss.to_string(),
            aud: aud.to_string(),
            exp,
            nbf,
            serviceurl: Some("https://smba.trafficmanager.net/teams/".to_string()),
        };
        let key = EncodingKey::from_rsa_pem(TEST_KEY_PEM.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    fn future_exp() -> i64 {
        chrono::Utc::now().timestamp() + 3600
    }

    fn jwks_body(kid: &str, n: &str) -> serde_json::Value {
        serde_json::json!({
            "keys": [
                { "kty": "RSA", "use": "sig", "kid": kid, "n": n, "e": "AQAB", "endorsements": ["msteams", "directline"] },
                { "kty": "EC", "use": "sig", "kid": "ec-key-ignored" }
            ]
        })
    }

    async fn mock_issuer(server: &MockServer, kid: &str) {
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body(kid, TEST_KEY_N)))
            .mount(server)
            .await;
    }

    fn validator(server: &MockServer) -> JwtValidator {
        JwtValidator::new(format!("{}/metadata", server.uri()))
            .with_refresh_min_interval(Duration::ZERO)
    }

    fn issuers() -> Vec<String> {
        connector_issuers()
    }

    #[tokio::test]
    async fn valid_token_is_accepted_and_serviceurl_and_endorsements_surfaced() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));
        let claims = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();
        assert_eq!(
            claims.serviceurl.as_deref(),
            Some("https://smba.trafficmanager.net/teams/")
        );
        // Endorsements from the signing key are surfaced so the listener
        // can bind them to the activity's channelId.
        assert!(claims.endorsements.iter().any(|e| e == "msteams"));
    }

    #[tokio::test]
    async fn token_not_yet_valid_is_rejected() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        // `nbf` sits past the clock-skew leeway in the future.
        let nbf = chrono::Utc::now().timestamp() + (JWT_CLOCK_SKEW_LEEWAY_SECS as i64) + 100;
        let token = mint_with_nbf(
            BOT_FRAMEWORK_ISSUER,
            APP_ID,
            future_exp(),
            Some(nbf),
            Some(TEST_KID),
        );
        let err = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Rejected(ref e)
                if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ImmatureSignature)
        ));
    }

    #[tokio::test]
    async fn token_valid_within_nbf_leeway_is_accepted() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        // `nbf` a little in the future but inside the skew leeway: accepted.
        let nbf = chrono::Utc::now().timestamp() + 30;
        let token = mint_with_nbf(
            BOT_FRAMEWORK_ISSUER,
            APP_ID,
            future_exp(),
            Some(nbf),
            Some(TEST_KID),
        );
        validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let exp = chrono::Utc::now().timestamp() - (JWT_CLOCK_SKEW_LEEWAY_SECS as i64) - 100;
        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, exp, Some(TEST_KID));
        let err = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Rejected(ref e)
                if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ExpiredSignature)
        ));
    }

    #[tokio::test]
    async fn wrong_audience_is_rejected() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let token = mint(
            BOT_FRAMEWORK_ISSUER,
            "some-other-app",
            future_exp(),
            Some(TEST_KID),
        );
        let err = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Rejected(ref e)
                if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::InvalidAudience)
        ));
    }

    #[tokio::test]
    async fn wrong_issuer_is_rejected() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let token = mint(
            "https://evil.example.invalid",
            APP_ID,
            future_exp(),
            Some(TEST_KID),
        );
        let err = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Rejected(ref e)
                if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::InvalidIssuer)
        ));
    }

    #[tokio::test]
    async fn tampered_payload_fails_signature_check() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));
        let mut parts: Vec<&str> = token.split('.').collect();
        // Re-encode the payload with a different audience; the signature
        // no longer matches.
        use base64::Engine as _;
        let engine = &base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut payload: serde_json::Value =
            serde_json::from_slice(&engine.decode(parts[1]).unwrap()).unwrap();
        payload["aud"] = serde_json::Value::String(APP_ID.to_string());
        payload["scope"] = serde_json::Value::String("escalated".to_string());
        let tampered_payload = engine.encode(serde_json::to_vec(&payload).unwrap());
        parts[1] = &tampered_payload;
        let tampered = parts.join(".");

        let err = validator(&server)
            .validate(&tampered, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Rejected(ref e)
                if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::InvalidSignature)
        ));
    }

    #[tokio::test]
    async fn garbage_token_is_malformed() {
        let server = MockServer::start().await;
        let err = validator(&server)
            .validate("not-a-jwt", APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::MalformedToken(_)));
    }

    #[tokio::test]
    async fn token_without_kid_is_rejected_before_any_fetch() {
        let server = MockServer::start().await;
        // No mocks mounted: reaching the network would 404 loudly.
        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), None);
        let err = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::MissingKeyId));
    }

    #[tokio::test]
    async fn unknown_kid_is_rejected_after_refresh() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some("rotated"));
        let err = validator(&server)
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::UnknownKeyId(ref kid) if kid == "rotated"));
    }

    #[tokio::test]
    async fn key_rotation_triggers_jwks_refetch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(&server)
            .await;
        // First JWKS fetch serves the old kid, the second the rotated one.
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(jwks_body("old-kid", TEST_KEY_N)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body(TEST_KID, TEST_KEY_N)))
            .mount(&server)
            .await;

        let validator = validator(&server);
        let old = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some("old-kid"));
        validator.validate(&old, APP_ID, &issuers()).await.unwrap();

        let rotated = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));
        validator
            .validate(&rotated, APP_ID, &issuers())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn jwks_refresh_rate_limit_skips_refetch() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        // Production interval: the second unknown kid inside the window
        // must NOT trigger another fetch pair.
        let validator = JwtValidator::new(format!("{}/metadata", server.uri()));
        let good = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));
        validator.validate(&good, APP_ID, &issuers()).await.unwrap();

        let unknown = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some("rotated"));
        let err = validator
            .validate(&unknown, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::UnknownKeyId(_)));
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path() == "/keys")
                .count(),
            1,
            "unknown kid within the refresh window must not re-fetch the JWKS"
        );
    }

    #[test]
    fn bearer_token_extraction() {
        assert_eq!(bearer_token("Bearer abc.def.ghi"), Some("abc.def.ghi"));
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("bearer abc"), None);
        assert_eq!(bearer_token("Basic dXNlcjpwYXNz"), None);
        assert_eq!(bearer_token(""), None);
    }

    #[test]
    fn connector_issuers_is_bot_framework_only() {
        // Connector-to-bot tokens are always minted by the Bot Framework
        // issuer; tenant Entra issuers must not be accepted here.
        assert_eq!(connector_issuers(), vec![BOT_FRAMEWORK_ISSUER.to_string()]);
    }

    #[tokio::test]
    async fn stale_cache_refetches_even_for_known_kid() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body(TEST_KID, TEST_KEY_N)))
            .mount(&server)
            .await;

        // Zero max-age forces a fresh fetch on every validate, even though
        // the kid is already cached — this is the daily-refresh bound that
        // drops keys the issuer has withdrawn.
        let validator = JwtValidator::new(format!("{}/metadata", server.uri()))
            .with_refresh_min_interval(Duration::ZERO)
            .with_max_age(Duration::ZERO);

        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));
        validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();
        validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();

        let key_fetches = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/keys")
            .count();
        assert!(
            key_fetches >= 2,
            "a stale cache must re-fetch the JWKS even when the kid is known, got {key_fetches}"
        );
    }

    /// Metadata resolves, but the JWKS document itself is broken.
    async fn mock_failing_issuer(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(500))
            .mount(server)
            .await;
    }

    fn path_hits(requests: &[wiremock::Request], path: &str) -> usize {
        requests.iter().filter(|r| r.url.path() == path).count()
    }

    /// Marks the resolver's client so the server can tell which client
    /// actually made a request. A held client would carry no marker.
    const EGRESS_PROBE_UA: &str = "zeroclaw-auth-egress-probe";

    fn probe_resolver() -> HttpClientResolver {
        Arc::new(|| {
            reqwest::Client::builder()
                .user_agent(EGRESS_PROBE_UA)
                .build()
                .expect("probe client builds")
        })
    }

    fn paths_from_the_probe(requests: &[wiremock::Request]) -> Vec<String> {
        requests
            .iter()
            .filter(|r| {
                r.headers
                    .get(reqwest::header::USER_AGENT)
                    .and_then(|value| value.to_str().ok())
                    == Some(EGRESS_PROBE_UA)
            })
            .map(|r| r.url.path().to_string())
            .collect()
    }

    /// A deployment behind a corporate proxy configures it once, on the
    /// channel; if the JWKS fetch keeps its own client it dials Microsoft
    /// directly and every inbound activity fails authentication while
    /// outbound sends look fine.
    #[tokio::test]
    async fn jwks_fetch_leaves_through_the_resolved_client() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        let validator = JwtValidator::new(format!("{}/metadata", server.uri()))
            .with_http_client_resolver(probe_resolver());
        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));
        validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();

        assert_eq!(
            paths_from_the_probe(&server.received_requests().await.unwrap()),
            vec!["/metadata".to_string(), "/keys".to_string()],
            "both legs of the refresh must use the channel's client"
        );
    }

    /// Same for the Entra token: without it a proxied deployment cannot
    /// mint a Connector token, so no reply goes out either.
    #[tokio::test]
    async fn connector_token_leaves_through_the_resolved_client() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-1",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;

        let provider = ConnectorTokenProvider::new(format!("{}/token", server.uri()))
            .with_http_client_resolver(probe_resolver());
        assert_eq!(provider.token("app-1", "secret-1").await.unwrap(), "tok-1");

        assert_eq!(
            paths_from_the_probe(&server.received_requests().await.unwrap()),
            vec!["/token".to_string()],
            "the token request must use the channel's client"
        );
    }

    #[tokio::test]
    async fn failed_refresh_attempts_are_rate_limited() {
        let server = MockServer::start().await;
        mock_failing_issuer(&server).await;

        // Production interval. Unknown key ids are attacker-controlled and
        // reach this path before any signature check, so a failing issuer
        // must not be re-probed once per inbound request.
        let validator = JwtValidator::new(format!("{}/metadata", server.uri()));
        let unknown = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some("rotated"));

        let first = validator
            .validate(&unknown, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(
            matches!(first, AuthError::JwksFetch(_)),
            "the attempt that actually failed should surface the fetch error, got {first:?}"
        );

        for _ in 0..5 {
            let err = validator
                .validate(&unknown, APP_ID, &issuers())
                .await
                .unwrap_err();
            assert!(
                matches!(err, AuthError::KeysUnavailable),
                "a throttled request has no trusted key set to judge against, got {err:?}"
            );
        }

        // Both endpoints are counted: asserting only the second one would
        // still pass if the attempt were stamped after the metadata lookup,
        // which would leave that lookup unthrottled.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            (
                path_hits(&requests, "/metadata"),
                path_hits(&requests, "/keys")
            ),
            (1, 1),
            "repeated unknown key ids during a failing refresh must attempt only once per interval"
        );
    }

    #[tokio::test]
    async fn failed_metadata_lookup_is_rate_limited() {
        let server = MockServer::start().await;
        // The refresh dies at the first of the two Microsoft endpoints, so
        // the JWKS document is never reached.
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let validator = JwtValidator::new(format!("{}/metadata", server.uri()));
        let unknown = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some("rotated"));

        let first = validator
            .validate(&unknown, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(
            matches!(first, AuthError::JwksFetch(_)),
            "the attempt that actually failed should surface the fetch error, got {first:?}"
        );

        for _ in 0..5 {
            let err = validator
                .validate(&unknown, APP_ID, &issuers())
                .await
                .unwrap_err();
            assert!(
                matches!(err, AuthError::KeysUnavailable),
                "a throttled request has no trusted key set to judge against, got {err:?}"
            );
        }

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            (
                path_hits(&requests, "/metadata"),
                path_hits(&requests, "/keys")
            ),
            (1, 0),
            "a failing metadata lookup must be throttled like a failing JWKS fetch"
        );
    }

    /// A burst against an empty cache, as on startup or once the previous
    /// key set passes [`JWKS_MAX_AGE`]. Several callers read the cache
    /// concurrently, all miss, and all reach for the refresh; only one wins
    /// and the rest are throttled. Being throttled behind a refresh that
    /// succeeded is not grounds for rejection: the key set those callers
    /// were waiting for is cached and within its age bound by the time they
    /// get the lock.
    ///
    /// Whether the callers overlap is up to the scheduler, and a single
    /// burst can be serialized into the winner finishing before the rest
    /// start, which exercises the cache fast path instead. Each round is an
    /// independent chance at the overlap, so several of them make a missed
    /// regression vanishingly unlikely. No round can fail against correct
    /// behaviour, where the whole burst is served off the one refresh.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_first_validates_share_one_refresh() {
        const BURST: usize = 16;
        const ROUNDS: usize = 5;

        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;
        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));

        for _ in 0..ROUNDS {
            // Fresh per round: the empty cache is what sends the whole
            // burst down the refresh path. Production refresh interval, so
            // only the winner's attempt fits in the window and every other
            // caller is throttled.
            let validator = Arc::new(JwtValidator::new(format!("{}/metadata", server.uri())));
            // Released together so the callers overlap on the cache read
            // rather than trickling in behind a refresh that has already
            // finished.
            let start = Arc::new(tokio::sync::Barrier::new(BURST));
            let mut burst = tokio::task::JoinSet::new();
            for _ in 0..BURST {
                let validator = validator.clone();
                let token = token.clone();
                let start = start.clone();
                burst.spawn(async move {
                    start.wait().await;
                    validator.validate(&token, APP_ID, &issuers()).await
                });
            }
            while let Some(joined) = burst.join_next().await {
                joined.unwrap().expect(
                    "a validate throttled behind a successful refresh must still be served",
                );
            }
        }

        assert_eq!(
            path_hits(&server.received_requests().await.unwrap(), "/keys"),
            ROUNDS,
            "each round's burst must collapse into a single JWKS fetch"
        );
    }

    #[tokio::test]
    async fn over_age_cache_is_unusable_when_refresh_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(&server)
            .await;
        // The key set loads once, then the issuer breaks.
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body(TEST_KID, TEST_KEY_N)))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        // Zero max-age: the cache is over-age immediately after loading, so
        // the mandatory daily refresh applies to every validate.
        let validator = JwtValidator::new(format!("{}/metadata", server.uri()))
            .with_refresh_min_interval(Duration::ZERO)
            .with_max_age(Duration::ZERO);
        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));

        validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();

        // `TEST_KID` is still in the cache, but the cache is past its age
        // bound and the refresh that would renew it failed, so the token
        // must be rejected instead of validated against retained keys.
        let err = validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(
            matches!(err, AuthError::JwksFetch(_)),
            "an over-age cache must not validate a cached kid after a failed refresh, got {err:?}"
        );
    }

    #[tokio::test]
    async fn over_age_cache_is_unusable_when_refresh_is_throttled() {
        let server = MockServer::start().await;
        mock_issuer(&server, TEST_KID).await;

        // Production refresh interval with a zero max-age: the first
        // validate loads the keys and closes the refresh window, so the
        // second one finds an over-age cache it is not allowed to renew.
        // Throttling refresh attempts must not smuggle the retained keys
        // back into use.
        let validator =
            JwtValidator::new(format!("{}/metadata", server.uri())).with_max_age(Duration::ZERO);
        let token = mint(BOT_FRAMEWORK_ISSUER, APP_ID, future_exp(), Some(TEST_KID));

        validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap();

        let err = validator
            .validate(&token, APP_ID, &issuers())
            .await
            .unwrap_err();
        assert!(
            matches!(err, AuthError::KeysUnavailable),
            "a throttled mandatory refresh must leave the over-age cache unusable, got {err:?}"
        );
        assert_eq!(
            path_hits(&server.received_requests().await.unwrap(), "/keys"),
            1,
            "the throttled validate must not have issued another JWKS request"
        );
    }

    #[tokio::test]
    async fn connector_token_is_fetched_and_cached() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=client_credentials"))
            .and(body_string_contains("client_id=app-1"))
            .and(body_string_contains("client_secret=secret-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_type": "Bearer",
                "access_token": "tok-1",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = ConnectorTokenProvider::new(format!("{}/token", server.uri()));
        assert_eq!(provider.token("app-1", "secret-1").await.unwrap(), "tok-1");
        // Second call must come from the cache (the mock allows one hit).
        assert_eq!(provider.token("app-1", "secret-1").await.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn rotated_credentials_mint_a_new_connector_token() {
        let server = MockServer::start().await;
        // Distinct bodies per credential pair, so the assertions below prove
        // which pair each token was minted for rather than just counting.
        for (secret, token) in [("secret-1", "tok-1"), ("secret-2", "tok-2")] {
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(body_string_contains("client_id=app-1"))
                .and(body_string_contains(format!("client_secret={secret}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": token,
                    "expires_in": 3600,
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("client_id=app-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-other-app",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;

        let provider = ConnectorTokenProvider::new(format!("{}/token", server.uri()));
        assert_eq!(provider.token("app-1", "secret-1").await.unwrap(), "tok-1");
        assert_eq!(
            provider.token("app-1", "secret-1").await.unwrap(),
            "tok-1",
            "unchanged credentials must still be served from the cache"
        );

        // Rotated secret, same tenant and app: the cached token is still
        // inside its hour, but it belongs to the retired secret.
        assert_eq!(
            provider.token("app-1", "secret-2").await.unwrap(),
            "tok-2",
            "a rotated secret must not keep sending the token minted for the old one"
        );
        // Same again for a swapped bot identity, which would otherwise post
        // as the previous app until the hour ran out.
        assert_eq!(
            provider.token("app-2", "secret-2").await.unwrap(),
            "tok-other-app",
            "a changed app id must not keep sending the previous app's token"
        );

        let posts = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == "/token")
            .count();
        assert_eq!(posts, 3, "one mint per distinct credential pair");
    }

    #[tokio::test]
    async fn connector_token_inside_refresh_margin_is_refetched() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-short",
                "expires_in": 10,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-fresh",
                "expires_in": 3600,
            })))
            .mount(&server)
            .await;

        let provider = ConnectorTokenProvider::new(format!("{}/token", server.uri()));
        assert_eq!(
            provider.token("app-1", "secret-1").await.unwrap(),
            "tok-short"
        );
        // 10s lifetime is inside the 300s refresh margin: refetch.
        assert_eq!(
            provider.token("app-1", "secret-1").await.unwrap(),
            "tok-fresh"
        );
    }

    #[tokio::test]
    async fn connector_token_endpoint_error_is_surfaced() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string(r#"{"error":"invalid_client"}"#),
            )
            .mount(&server)
            .await;

        let provider = ConnectorTokenProvider::new(format!("{}/token", server.uri()));
        let err = provider.token("app-1", "wrong").await.unwrap_err();
        assert!(matches!(
            err,
            AuthError::TokenEndpoint { status: 401, ref body } if body.contains("invalid_client")
        ));
    }

    #[test]
    fn connector_token_url_targets_tenant() {
        assert_eq!(
            connector_token_url("tenant-123"),
            "https://login.microsoftonline.com/tenant-123/oauth2/v2.0/token"
        );
    }

    #[test]
    fn debug_output_redacts_cached_token() {
        let provider = ConnectorTokenProvider::new("https://example.invalid/token");
        let out = format!("{provider:?}");
        assert!(out.contains("<redacted>"));
    }
}
