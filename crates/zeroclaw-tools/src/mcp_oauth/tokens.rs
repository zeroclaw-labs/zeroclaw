//! Token endpoint operations: authorization-code exchange (RFC 6749 §4.1.3)
//! and refresh (RFC 6749 §6), each carrying the RFC 8707 `resource` parameter
//! bound to the canonical MCP resource, and a failure taxonomy the transport
//! layer can act on without parsing strings.
//!
//! Tokens never appear in error text: a token endpoint response body is
//! token material, so malformed-success errors are reported without body
//! content.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;

use super::metadata::ValidatedAs;
use super::policy::MAX_METADATA_BYTES;
use super::resource::CanonicalResource;

/// The shape of a token-endpoint failure, classified so callers can map it
/// to "needs the browser again" vs "retry later" vs "configuration is wrong"
/// without matching on server strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenErrorKind {
    /// The grant (code or refresh token) is dead or was rejected: the flow
    /// must be redone from the authorization endpoint.
    ReauthorizationRequired,
    /// The client or the request is wrong in a way a retry cannot fix.
    Permanent,
    /// The server or network is unhappy; retrying later may succeed.
    Transient,
}

/// Structured token-endpoint failure. Carried as an anyhow context type so
/// the transport slice can `downcast_ref` it while the Display text stays
/// actionable and token-free.
#[derive(Debug, Clone)]
pub struct TokenEndpointError {
    pub kind: TokenErrorKind,
    pub status: u16,
    pub code: Option<String>,
    pub description: Option<String>,
}

impl std::fmt::Display for TokenEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "token endpoint returned HTTP {}", self.status)?;
        // Both fields are server-supplied and unbounded; cap before they
        // reach a console or log line.
        if let Some(code) = &self.code {
            write!(f, " {}", super::truncate_untrusted(code))?;
        }
        if let Some(description) = &self.description {
            write!(f, " ({})", super::truncate_untrusted(description))?;
        }
        Ok(())
    }
}

impl std::error::Error for TokenEndpointError {}

/// RFC 6749 §5.2 error bodies; servers are loose, so every member is
/// optional and non-JSON bodies are tolerated.
#[derive(Deserialize)]
struct TokenErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// RFC 6749 §5.1 / §5.3 success body, mapped onto the shared
/// [`zeroclaw_providers::auth::profiles::TokenSet`].
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    /// RFC 6749 §5.1 says a JSON number, but real servers emit `"3600"` and
    /// `3600.0` (S2-4); accepting the coercible shapes here keeps a merely
    /// sloppy server from failing the whole exchange — anything else means
    /// "no lifetime", which is the safe reading.
    #[serde(default)]
    expires_in: Option<serde_json::Value>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// Lenient `expires_in` coercion (S2-4): integer, finite float, or numeric
/// string; anything else → `None`.
fn expires_in_secs(value: Option<&serde_json::Value>) -> Option<i64> {
    match value? {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        serde_json::Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Re-export of the shared provider token set. WARNING: its derived `Debug`
/// prints all three tokens raw. Everything in this module that hands one
/// around (`McpAuthorization`, the manager's `Grant`) implements a redacted
/// `Debug` by hand; never `{:?}` a bare `TokenSet` in a production path.
pub type TokenSet = zeroclaw_providers::auth::profiles::TokenSet;

/// Classify a token-endpoint failure response: the error code leads, the
/// status narrows what is left. Anything not provably permanent is treated
/// as transient.
///
/// Grant deletion is narrower than this table (design §12): a stored grant
/// may only be discarded when the kind is `ReauthorizationRequired` AND the
/// status is 4xx — a 429/5xx that happens to carry `invalid_grant` stays
/// transient at the caller. A configured `oauth_client_id` additionally
/// downgrades `invalid_client`/`unauthorized_client` to Permanent at the
/// caller: a typo in config must not loop the browser.
fn classify(status: u16, code: Option<&str>) -> TokenErrorKind {
    // RFC 6749 §5.2: invalid_grant is THE "the grant is dead" code;
    // invalid_client/unauthorized_client mean our stored/registered client
    // can no longer redeem anything, which in a public-client DCR model also
    // means re-running the flow (re-registration included).
    if matches!(
        code,
        Some("invalid_grant" | "invalid_client" | "unauthorized_client")
    ) {
        return TokenErrorKind::ReauthorizationRequired;
    }
    if status == 429 || (500..=599).contains(&status) {
        return TokenErrorKind::Transient;
    }
    // 401 with WWW-Authenticate on the token endpoint is rare (RFC 6750
    // reserves it for resource requests); treat bare 401 as reauthorization
    // since the token we presented was refused.
    if status == 401 {
        return TokenErrorKind::ReauthorizationRequired;
    }
    if matches!(
        code,
        Some(
            "invalid_request"
                | "invalid_scope"
                | "unsupported_grant_type"
                | "unsupported_token_type"
        )
    ) {
        return TokenErrorKind::Permanent;
    }
    if (400..=499).contains(&status) {
        return TokenErrorKind::Permanent;
    }
    TokenErrorKind::Transient
}

/// POST a form-encoded request to the validated token endpoint and map the
/// response to a success taxonomy: a usable [`TokenSet`], or
/// [`anyhow::Error`] carrying [`TokenEndpointError`] (error responses) or a
/// bare message (malformed success, never echoing token material).
async fn token_request(
    client: &reqwest::Client,
    auth_server: &ValidatedAs,
    form: &[(&str, String)],
    what: &str,
) -> Result<TokenSet> {
    let response = client
        .post(auth_server.token_endpoint.as_str())
        // RFC 6749 §5.1 speaks JSON; ask for it explicitly so servers that
        // content-negotiate do not answer form-encoded or HTML bodies (S2-3).
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .with_context(|| format!("failed to reach the token endpoint for {what}"))?;
    let status = response.status();
    let body = crate::mcp_transport::read_body_bounded_to_string(response, MAX_METADATA_BYTES)
        .await
        .with_context(|| format!("failed to read the token response for {what}"))?;

    if !status.is_success() {
        let parsed = serde_json::from_str::<TokenErrorBody>(&body).ok();
        let (code, description) = match parsed {
            Some(body) => (body.error, body.error_description),
            None => (None, None),
        };
        let kind = classify(status.as_u16(), code.as_deref());
        // The body may itself be an HTML page or partial token material on a
        // broken server; only the structured RFC fields are surfaced.
        bail!(TokenEndpointError {
            kind,
            status: status.as_u16(),
            code,
            description,
        });
    }

    // Success body is token material: report malformed shapes structurally,
    // never with body content.
    let parsed: TokenResponse = serde_json::from_str(&body).map_err(|_| {
        anyhow::Error::msg(format!(
            "token endpoint returned HTTP {status} for {what} with a body that is not a usable token response"
        ))
    })?;
    if parsed.access_token.is_empty() {
        bail!("token endpoint returned an empty access_token for {what}");
    }
    if let Some(token_type) = parsed.token_type.as_deref()
        && !token_type.eq_ignore_ascii_case("bearer")
    {
        bail!(
            "token endpoint returned token_type {:?} for {what}; only bearer tokens are supported",
            super::truncate_untrusted(token_type)
        );
    }
    // RFC 6749 §6 allows the AS to omit `refresh_token` on refresh (the
    // loopback profile guidance in RFC 8252 §4.1.3/§8.1 assumes it):
    // an ABSENT one means "keep the stored one". The empty string is not a
    // token; storing it would poison the grant (S2-2), so it is normalized
    // to absence here too. The caller keeps the old token on `None`.
    let refresh_token = parsed.refresh_token.filter(|token| !token.is_empty());
    let expires_at = expires_at_for(expires_in_secs(parsed.expires_in.as_ref()));
    Ok(TokenSet {
        access_token: parsed.access_token,
        refresh_token,
        id_token: parsed.id_token,
        expires_at,
        token_type: Some("Bearer".to_string()),
        scope: parsed.scope,
    })
}

/// Exchange an authorization code for tokens. `client` MUST come from
/// `OAuthClientFactory::client_for(&auth_server.token_endpoint)` — the
/// endpoint is AS-supplied and only the per-target client carries the URL
/// policy + DNS pinning. `redirect_uri` must be the exact URI the code was
/// issued for, and `code_verifier` the flow's verifier; `resource` is added
/// from the canonical resource (RFC 8707).
pub async fn exchange_code(
    client: &reqwest::Client,
    auth_server: &ValidatedAs,
    canonical: &CanonicalResource,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> Result<TokenSet> {
    let form = [
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", client_id.to_string()),
        ("code_verifier", code_verifier.to_string()),
        ("resource", canonical.resource().to_string()),
    ];
    token_request(
        client,
        auth_server,
        &form,
        "the authorization-code exchange",
    )
    .await
}

/// Refresh a token set. `client` MUST come from
/// `OAuthClientFactory::client_for(&auth_server.token_endpoint)` (same
/// per-target pinning requirement as [`exchange_code`]).
/// `current_refresh_token` is carried into the result
/// when the server omits `refresh_token` (RFC 6749 §6: the old refresh token
/// remains valid in that case).
pub async fn refresh_tokens(
    client: &reqwest::Client,
    auth_server: &ValidatedAs,
    canonical: &CanonicalResource,
    client_id: &str,
    current_refresh_token: &str,
) -> Result<(TokenSet, String)> {
    let form = [
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", current_refresh_token.to_string()),
        ("client_id", client_id.to_string()),
        ("resource", canonical.resource().to_string()),
    ];
    let mut tokens = token_request(client, auth_server, &form, "the token refresh").await?;
    let refresh_token = tokens
        .refresh_token
        .clone()
        .unwrap_or_else(|| current_refresh_token.to_string());
    tokens.refresh_token = Some(refresh_token.clone());
    Ok((tokens, refresh_token))
}

/// The `Authorization: Bearer` value to attach to MCP requests. Kept here so
/// the exact spelling has one owner.
pub fn bearer_header_value(access_token: &str) -> String {
    format!("Bearer {access_token}")
}

/// Resolve `expires_in` (RFC 6749 §5.1) into an expiry instant. Absent
/// stays absent (no lifetime; only a 401 renews). Zero — or a negative, on
/// the strict "seconds remaining" reading — means the grant is ALREADY
/// expired, not never-expiring. A value overflowing chrono's range degrades
/// to absent rather than a wrong far-past instant.
fn expires_at_for(expires_in: Option<i64>) -> Option<DateTime<Utc>> {
    match expires_in {
        None => None,
        Some(secs) if secs <= 0 => Some(Utc::now()),
        Some(secs) => TimeDelta::try_seconds(secs)
            // `try_seconds` only checks that the DURATION fits; the SUM must
            // also stay inside chrono's representable range, and an
            // untrusted `expires_in` from the AS must degrade to "absent"
            // (the documented contract of this fn), never panic.
            .and_then(|delta| Utc::now().checked_add_signed(delta)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mock_as(issuer: &str) -> ValidatedAs {
        ValidatedAs {
            issuer: Url::parse(issuer).expect("issuer"),
            authorization_endpoint: Url::parse(&format!("{issuer}/authorize")).expect("url"),
            token_endpoint: Url::parse(&format!("{issuer}/token")).expect("url"),
            registration_endpoint: None,
            scopes_supported: None,
            require_iss: false,
        }
    }

    fn canonical() -> CanonicalResource {
        CanonicalResource::from_configured(&Url::parse("http://127.0.0.1:7777/mcp").expect("url"))
            .expect("canonical")
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
    }

    async fn mock_with_token_as(body: ResponseTemplate) -> (MockServer, ValidatedAs) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(body)
            .mount(&server)
            .await;
        let auth_server = mock_as(&server.uri()).await;
        (server, auth_server)
    }

    #[tokio::test]
    async fn token_requests_ask_for_json() {
        // S2-3: the token endpoint must be asked (Accept) for the JSON
        // media type RFC 6749 §5.1 defines, not left to negotiate.
        let (server, auth_server) = mock_with_token_as(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "at"})),
        )
        .await;
        exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "c",
            "v",
        )
        .await
        .expect("exchange");
        let request = server
            .received_requests()
            .await
            .expect("requests")
            .into_iter()
            .find(|r| r.url.path() == "/token")
            .expect("token request");
        assert_eq!(
            request
                .headers
                .get(reqwest::header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn sloppy_expires_in_shapes_do_not_fail_the_exchange() {
        // S2-4: integer (baseline, other tests), quoted and float shapes
        // must parse; garbage means "no lifetime", never a failed exchange.
        for (shape, has_lifetime) in [
            (serde_json::json!("3600"), true),
            (serde_json::json!(3600.0), true),
            (serde_json::json!(" 7200 "), true),
            (serde_json::json!("soon"), false),
            (serde_json::json!({"a": 1}), false),
            (serde_json::json!(null), false),
        ] {
            let (_server, auth_server) = mock_with_token_as(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": "at", "expires_in": shape})),
            )
            .await;
            let tokens = exchange_code(
                &client(),
                &auth_server,
                &canonical(),
                "cid",
                "http://127.0.0.1:9/callback",
                "c",
                "v",
            )
            .await
            .unwrap_or_else(|err| panic!("shape {shape:?} must not fail the exchange: {err}"));
            assert_eq!(
                tokens.expires_at.is_some(),
                has_lifetime,
                "shape {shape:?} lifetime classification"
            );
        }
    }

    #[tokio::test]
    async fn empty_refresh_token_is_normalized_to_absent() {
        // S2-2: "" is not a refresh token; storing it would poison the
        // grant. It normalizes to None, which the store/manager then treat
        // as "the old token stands".
        let (_server, auth_server) = mock_with_token_as(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"access_token": "at", "refresh_token": ""})),
        )
        .await;
        let tokens = exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "c",
            "v",
        )
        .await
        .expect("exchange");
        assert_eq!(tokens.refresh_token, None, "empty refresh_token poisoned");
    }

    #[tokio::test]
    async fn exchange_sends_rfc_shape_with_resource_and_verifier() {
        let (_server, auth_server) =
            mock_with_token_as(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "expires_in": 3600,
                "token_type": "Bearer",
                "scope": "openid mcp",
            })))
            .await;
        let tokens = exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "the-code",
            "the-verifier",
        )
        .await
        .expect("tokens");
        assert_eq!(tokens.access_token, "at-1");
        assert!(tokens.expires_at.is_some());
        assert_eq!(tokens.scope.as_deref(), Some("openid mcp"));

        let requests = _server.received_requests().await.expect("received");
        let body = String::from_utf8(requests[0].body.clone()).expect("utf8");
        for expected in [
            "grant_type=authorization_code",
            "code=the-code",
            "code_verifier=the-verifier",
            "client_id=cid",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcallback",
            "resource=http%3A%2F%2F127.0.0.1%3A7777%2Fmcp",
        ] {
            assert!(
                body.contains(expected),
                "form body missing {expected}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_grant_is_classified_as_reauthorization_required() {
        let (_server, auth_server) = mock_with_token_as(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "invalid_grant"})),
        )
        .await;
        let err = exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "expired-code",
            "v",
        )
        .await
        .expect_err("must fail");
        let structured = err
            .downcast_ref::<TokenEndpointError>()
            .expect("typed error");
        assert_eq!(structured.kind, TokenErrorKind::ReauthorizationRequired);
        assert_eq!(structured.code.as_deref(), Some("invalid_grant"));
        assert_eq!(structured.status, 400);
    }

    #[tokio::test]
    async fn server_5xx_is_transient_and_4xx_without_code_is_permanent() {
        for (status, kind) in [
            (500, TokenErrorKind::Transient),
            (503, TokenErrorKind::Transient),
            (429, TokenErrorKind::Transient),
            (404, TokenErrorKind::Permanent),
            (400, TokenErrorKind::Permanent),
            (401, TokenErrorKind::ReauthorizationRequired),
        ] {
            let (_server, auth_server) =
                mock_with_token_as(ResponseTemplate::new(status).set_body_string("nope")).await;
            let err = exchange_code(
                &client(),
                &auth_server,
                &canonical(),
                "cid",
                "http://127.0.0.1:9/callback",
                "c",
                "v",
            )
            .await
            .expect_err("must fail");
            let structured = err
                .downcast_ref::<TokenEndpointError>()
                .unwrap_or_else(|| panic!("HTTP {status} must carry a typed error"));
            assert_eq!(structured.kind, kind, "HTTP {status} classified wrong");
        }
    }

    #[tokio::test]
    async fn malformed_success_never_leaks_token_material() {
        // A 200 whose body is not a token response: the raw body may contain
        // a token; the error must describe the shape, not the content.
        let (_server, auth_server) = mock_with_token_as(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"unexpected": "s3cr3t-access-token-value"}"#),
        )
        .await;
        let err = exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "c",
            "v",
        )
        .await
        .expect_err("must fail");
        assert!(
            !err.to_string().contains("s3cr3t-access-token-value"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn non_bearer_token_type_is_rejected() {
        let (_server, auth_server) =
            mock_with_token_as(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "token_type": "MAC"
            })))
            .await;
        let err = exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "c",
            "v",
        )
        .await
        .expect_err("must fail");
        assert!(err.to_string().contains("token_type"), "got: {err}");
    }

    #[tokio::test]
    async fn bearer_is_case_insensitive_on_the_wire() {
        let (_server, auth_server) =
            mock_with_token_as(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "token_type": "bearer"
            })))
            .await;
        let tokens = exchange_code(
            &client(),
            &auth_server,
            &canonical(),
            "cid",
            "http://127.0.0.1:9/callback",
            "c",
            "v",
        )
        .await
        .expect("bearer case-insensitive per RFC 6750");
        assert_eq!(tokens.token_type.as_deref(), Some("Bearer"));
    }

    #[tokio::test]
    async fn refresh_keeps_the_old_refresh_token_when_server_omits_one() {
        let (_server, auth_server) = mock_with_token_as(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": "at-2"})),
        )
        .await;
        let (tokens, refresh) =
            refresh_tokens(&client(), &auth_server, &canonical(), "cid", "rt-old")
                .await
                .expect("refreshed");
        assert_eq!(tokens.access_token, "at-2");
        assert_eq!(
            refresh, "rt-old",
            "RFC 6749 §6: the old refresh token stays valid"
        );
        let requests = _server.received_requests().await.expect("received");
        let body = String::from_utf8(requests[0].body.clone()).expect("utf8");
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=rt-old"));
        assert!(body.contains("resource=http%3A%2F%2F127.0.0.1%3A7777%2Fmcp"));
    }

    #[test]
    fn expires_in_zero_or_negative_means_already_expired() {
        assert!(expires_at_for(None).is_none());
        let now = chrono::Utc::now();
        for secs in [0, -1, -3600] {
            let at = expires_at_for(Some(secs)).expect("expired instant");
            assert!(at <= now + chrono::Duration::seconds(1));
            assert!(at >= now - chrono::Duration::seconds(60));
        }
        let future = expires_at_for(Some(3600)).expect("future instant");
        assert!(future > now + chrono::Duration::seconds(3590));
        // Overflowing values degrade to "unknown lifetime", never a wrong
        // instant.
        assert!(expires_at_for(Some(i64::MAX)).is_none());
    }

    #[test]
    fn expires_in_beyond_chrono_date_range_degrades_to_absent() {
        // 10^13 seconds (~317k years) fits inside a `TimeDelta`, so
        // `try_seconds` accepts it; the SUM with Utc::now() overflows
        // chrono's representable dates, and an untrusted AS response must
        // degrade to "absent", never panic at connect time.
        assert!(expires_at_for(Some(10_000_000_000_000)).is_none());
    }

    #[test]
    fn transient_status_with_invalid_grant_keeps_reauth_kind_for_status_gating() {
        // Design §12: classification stays code-first, and the CALLER gates
        // grant deletion on the 4xx pairing. Pin both halves of that
        // contract: the kind here, the status field on the typed error.
        assert_eq!(
            classify(503, Some("invalid_grant")),
            TokenErrorKind::ReauthorizationRequired
        );
        assert_eq!(
            classify(429, Some("invalid_grant")),
            TokenErrorKind::ReauthorizationRequired
        );
        assert_eq!(
            classify(400, Some("invalid_grant")),
            TokenErrorKind::ReauthorizationRequired
        );
        assert_eq!(classify(503, None), TokenErrorKind::Transient);
    }

    #[test]
    fn typed_error_display_bounds_server_text() {
        let err = TokenEndpointError {
            kind: TokenErrorKind::Permanent,
            status: 400,
            code: Some("bad_code".repeat(200)),
            description: Some("d".repeat(5000)),
        };
        let shown = err.to_string();
        assert!(
            shown.len() < 700,
            "display must bound untrusted fields: {}",
            shown.len()
        );
    }

    #[test]
    fn bearer_header_value_is_exact() {
        assert_eq!(bearer_header_value("tok"), "Bearer tok");
    }
}
