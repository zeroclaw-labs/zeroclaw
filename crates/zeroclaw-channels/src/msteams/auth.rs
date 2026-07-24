//! Bot Framework authentication for the Microsoft Teams channel.
//!
//! Two directions, two types:
//!
//! - **Inbound** — every activity POST from the Bot Connector service
//!   carries a `Authorization: Bearer <JWT>` header. [`JwtValidator`]
//!   verifies the RS256 signature against the issuer's JWKS document
//!   (discovered through OpenID metadata), plus issuer, audience
//!   (= the bot's `app_id`) and expiry, before any payload is trusted.
//! - **Outbound** — Connector API calls authenticate with an Entra
//!   client-credentials token. [`ConnectorTokenProvider`] fetches one and
//!   caches it until shortly before expiry.
//!
//! This is the only msteams module that touches key material or tokens.
//! Credentials are passed in per call (resolved from canonical `Config`
//! by the caller); neither type stores `app_id` / `app_password`.

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::collections::HashMap;
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

/// Do not re-fetch the JWKS document more often than this when a token
/// references an unknown `kid`. Bounds the damage of a flood of garbage
/// tokens each triggering an outbound fetch.
const JWKS_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Refresh the cached connector token this long before it expires, so an
/// outbound send never races token expiry mid-request.
const CONNECTOR_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);

/// Entra token endpoint (client-credentials flow) for a tenant.
#[must_use]
pub fn connector_token_url(tenant_id: &str) -> String {
    format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token")
}

/// Issuers accepted on inbound service tokens for a single-tenant bot:
/// the Bot Framework issuer plus the tenant's Entra v2 and v1 issuers
/// (which one mints the token depends on the bot's registration type).
#[must_use]
pub fn allowed_issuers(tenant_id: &str) -> Vec<String> {
    vec![
        BOT_FRAMEWORK_ISSUER.to_string(),
        format!("https://login.microsoftonline.com/{tenant_id}/v2.0"),
        format!("https://sts.windows.net/{tenant_id}/"),
    ]
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
    #[error("JWKS entry for key id {kid:?} is not a usable RSA key: {reason}")]
    UnusableJwk { kid: String, reason: String },
    #[error("JWT rejected: {0}")]
    Rejected(#[source] jsonwebtoken::errors::Error),
    #[error("token endpoint returned HTTP {status}: {body}")]
    TokenEndpoint { status: u16, body: String },
    #[error("token endpoint request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// Claims surfaced from a validated inbound service token. Issuer,
/// audience and expiry are enforced during validation and not re-exposed;
/// `serviceurl` is kept so the listener can cross-check it against the
/// activity's `serviceUrl` before replying there.
#[derive(Debug, Deserialize)]
pub struct ValidatedClaims {
    #[serde(default)]
    pub serviceurl: Option<String>,
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
}

#[derive(Default)]
struct JwksCache {
    /// `kid` -> RSA `(n, e)` components (base64url), as served by the
    /// issuer's JWKS document. Materialized view of the issuer's keys;
    /// refreshed on unknown-`kid` misses, never edited locally.
    keys: HashMap<String, (String, String)>,
    last_fetch: Option<Instant>,
}

/// Validates inbound Bot Connector service tokens against the issuer's
/// published JWKS.
pub struct JwtValidator {
    http: reqwest::Client,
    openid_metadata_url: String,
    jwks: tokio::sync::RwLock<JwksCache>,
    refresh_min_interval: Duration,
}

impl JwtValidator {
    /// `openid_metadata_url` is the OpenID configuration document whose
    /// `jwks_uri` serves the signing keys — normally
    /// [`BOT_FRAMEWORK_OPENID_METADATA_URL`].
    #[must_use]
    pub fn new(openid_metadata_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            openid_metadata_url: openid_metadata_url.into(),
            jwks: tokio::sync::RwLock::new(JwksCache::default()),
            refresh_min_interval: JWKS_REFRESH_MIN_INTERVAL,
        }
    }

    /// Test hook: allow immediate JWKS re-fetches so key-rotation paths
    /// can be exercised without waiting out the production rate limit.
    #[cfg(test)]
    fn with_refresh_min_interval(mut self, interval: Duration) -> Self {
        self.refresh_min_interval = interval;
        self
    }

    /// Validate a bearer token (without the `Bearer ` prefix).
    ///
    /// `app_id` is the expected audience; `issuers` the accepted issuer
    /// set (see [`allowed_issuers`]). Both are resolved from canonical
    /// config by the caller at call time.
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
        let key = self.decoding_key(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.leeway = JWT_CLOCK_SKEW_LEEWAY_SECS;
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        validation.set_audience(&[app_id]);
        validation.set_issuer(issuers);

        jsonwebtoken::decode::<ValidatedClaims>(token, &key, &validation)
            .map(|data| data.claims)
            .map_err(AuthError::Rejected)
    }

    /// Resolve `kid` from the cache, refreshing from the issuer once
    /// (rate-limited) on a miss to pick up rotated keys.
    async fn decoding_key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        if let Some(key) = self.cached_key(kid).await? {
            return Ok(key);
        }
        self.refresh_jwks().await?;
        match self.cached_key(kid).await? {
            Some(key) => Ok(key),
            None => Err(AuthError::UnknownKeyId(kid.to_string())),
        }
    }

    async fn cached_key(&self, kid: &str) -> Result<Option<DecodingKey>, AuthError> {
        let cache = self.jwks.read().await;
        let Some((n, e)) = cache.keys.get(kid) else {
            return Ok(None);
        };
        DecodingKey::from_rsa_components(n, e)
            .map(Some)
            .map_err(|err| AuthError::UnusableJwk {
                kid: kid.to_string(),
                reason: err.to_string(),
            })
    }

    async fn refresh_jwks(&self) -> Result<(), AuthError> {
        let mut cache = self.jwks.write().await;
        if cache
            .last_fetch
            .is_some_and(|last| last.elapsed() < self.refresh_min_interval)
        {
            // Recently refreshed; an unknown kid stays unknown rather
            // than triggering another outbound fetch.
            return Ok(());
        }

        let metadata: OpenIdMetadata = self
            .http
            .get(&self.openid_metadata_url)
            .send()
            .await
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?
            .error_for_status()
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?
            .json()
            .await
            .map_err(|err| AuthError::JwksFetch(err.to_string()))?;

        let jwks: JwksDocument = self
            .http
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
                (Some(kid), Some(n), Some(e)) => Some((kid, (n, e))),
                _ => None,
            })
            .collect();
        cache.last_fetch = Some(Instant::now());
        Ok(())
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
}

/// Fetches and caches the Entra client-credentials token used against the
/// Bot Connector API. The cached token is a time-bounded materialized
/// credential minted by Entra at runtime — the source of truth for the
/// *credentials* stays in config and is passed in per call.
pub struct ConnectorTokenProvider {
    http: reqwest::Client,
    token_url: String,
    cached: tokio::sync::RwLock<Option<CachedToken>>,
}

impl ConnectorTokenProvider {
    #[must_use]
    pub fn new(token_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            token_url: token_url.into(),
            cached: tokio::sync::RwLock::new(None),
        }
    }

    /// Provider for a tenant's production Entra token endpoint.
    #[must_use]
    pub fn for_tenant(tenant_id: &str) -> Self {
        Self::new(connector_token_url(tenant_id))
    }

    /// Return a bearer token for the Connector API, fetching a fresh one
    /// when none is cached or the cached one is inside the refresh margin.
    pub async fn token(&self, app_id: &str, app_password: &str) -> Result<String, AuthError> {
        if let Some(token) = self.fresh_cached_token().await {
            return Ok(token);
        }

        let mut cached = self.cached.write().await;
        // Another task may have refreshed while this one waited on the lock.
        if let Some(token) = cached.as_ref().filter(|t| Self::is_fresh(t)) {
            return Ok(token.access_token.clone());
        }

        let response = self
            .http
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
        });
        Ok(access_token)
    }

    async fn fresh_cached_token(&self) -> Option<String> {
        let cached = self.cached.read().await;
        cached
            .as_ref()
            .filter(|t| Self::is_fresh(t))
            .map(|t| t.access_token.clone())
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
        serviceurl: Option<String>,
    }

    fn mint(iss: &str, aud: &str, exp: i64, kid: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(str::to_string);
        let claims = TestClaims {
            iss: iss.to_string(),
            aud: aud.to_string(),
            exp,
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
                { "kty": "RSA", "use": "sig", "kid": kid, "n": n, "e": "AQAB" },
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
        vec![BOT_FRAMEWORK_ISSUER.to_string()]
    }

    #[tokio::test]
    async fn valid_token_is_accepted_and_serviceurl_surfaced() {
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
    fn allowed_issuers_cover_bot_framework_and_tenant() {
        let issuers = allowed_issuers("tenant-123");
        assert!(issuers.contains(&BOT_FRAMEWORK_ISSUER.to_string()));
        assert!(issuers.contains(&"https://login.microsoftonline.com/tenant-123/v2.0".to_string()));
        assert!(issuers.contains(&"https://sts.windows.net/tenant-123/".to_string()));
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
