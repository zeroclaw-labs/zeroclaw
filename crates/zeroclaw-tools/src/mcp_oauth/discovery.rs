//! OAuth discovery orchestration for one MCP resource.
//!
//! Given the canonical resource and the (optional) `WWW-Authenticate`
//! challenge captured from a 401, locate and validate the protected-resource
//! metadata (RFC 9728) and then the authorization-server metadata
//! (RFC 8414). Every URL here passes the shared policy and every document is
//! size/content-type bound before use. No credentials are involved at this
//! stage, so nothing here can leak token material.

use anyhow::{Context, Result, bail};
use reqwest::Url;

use super::challenge::parse_bearer_challenge;
use super::metadata::{
    ProtectedResourceMetadata, ValidatedAs, fetch_json_document, parse_document,
    validate_auth_server, validate_protected_resource,
};
use super::policy::{OAuthClientFactory, ensure_oauth_url, origin_key};
use super::resource::CanonicalResource;

/// A fully validated discovery result for one MCP resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDiscovery {
    pub canonical: CanonicalResource,
    /// Where the protected-resource document actually came from (challenge
    /// URL or RFC 9728 default), useful for actionable errors and caches.
    pub protected_resource_url: Url,
    pub auth_server: ValidatedAs,
}

/// Run discovery for `canonical`. `www_authenticate` is the raw
/// `WWW-Authenticate` header from the server's 401, when one was captured;
/// its `resource_metadata` parameter is used as the document URL only when
/// it parses and passes the URL policy, otherwise discovery falls back to the
/// RFC 9728 default URL, which is bound to the configured origin.
///
/// Every fetch uses a per-target client from `factory`, so each
/// server-supplied URL gets its own host validation and DNS pinning; the
/// factory must not be replaced by one shared client.
pub async fn discover_oauth(
    factory: &OAuthClientFactory,
    canonical: &CanonicalResource,
    www_authenticate: Option<&str>,
) -> Result<OAuthDiscovery> {
    let default_prm_url = canonical
        .well_known_prm_url()
        .context("cannot derive the protected-resource metadata URL")?;
    let prm_url = match www_authenticate
        .and_then(parse_bearer_challenge)
        .and_then(|challenge| challenge.resource_metadata)
    {
        Some(raw) if !raw.trim().is_empty() => {
            let hint = Url::parse(&raw)
                .ok()
                .filter(|url| ensure_oauth_url(url, "protected-resource metadata").is_ok());
            match hint {
                Some(url) => url,
                // Malformed or policy-violating hint: RFC 9728 does not force
                // us to trust it, and the default URL is safe and bound to
                // the configured origin. Fall back instead of hard-failing.
                None => default_prm_url,
            }
        }
        _ => default_prm_url,
    };

    let client = factory
        .client_for(&prm_url)
        .await
        .with_context(|| format!("cannot build a client for {prm_url}"))?;
    let document = fetch_json_document(&client, &prm_url, "protected-resource metadata").await?;
    let prm = parse_document::<ProtectedResourceMetadata>(document, "protected-resource metadata")?;
    let issuers = validate_protected_resource(prm, canonical)?;

    // Try each advertised authorization server; the first fully valid metadata
    // document wins. Failure reasons are accumulated, never short-circuited,
    // so the final error explains every candidate.
    let mut failures = Vec::new();
    for issuer in issuers {
        match discover_auth_server(factory, &issuer).await {
            Ok(auth_server) => {
                return Ok(OAuthDiscovery {
                    canonical: canonical.clone(),
                    protected_resource_url: prm_url,
                    auth_server,
                });
            }
            Err(err) => failures.push(format!("{issuer}: {err:#}")),
        }
    }
    bail!(
        "no advertised authorization server produced valid metadata for {} — {}",
        canonical.resource(),
        failures.join("; ")
    )
}

/// Fetch and validate AS metadata for one issuer, probing the RFC 8414 §3
/// well-known URL (origin-inserted, path preserved). The issuer's metadata
/// host may differ from the MCP host, so it gets its own pinned client.
async fn discover_auth_server(factory: &OAuthClientFactory, issuer: &Url) -> Result<ValidatedAs> {
    let metadata_url = auth_server_metadata_url(issuer)?;
    let client = factory
        .client_for(&metadata_url)
        .await
        .with_context(|| format!("cannot build a client for {metadata_url}"))?;
    let document = fetch_json_document(&client, &metadata_url, "authorization-server metadata")
        .await
        .map_err(|err| {
            // Keep the candidate's reason short: it is joined into the
            // aggregate discovery error.
            anyhow::Error::msg(format!("{err:#}"))
        })?;
    validate_auth_server(document, issuer)
}

/// RFC 8414 §3: insert `.well-known/oauth-authorization-server` between the
/// issuer's origin and its path (path preserved, trailing slash trimmed).
fn auth_server_metadata_url(issuer: &Url) -> Result<Url> {
    let origin = origin_key(issuer).context("authorization server issuer has no origin")?;
    let path = issuer.path().trim_end_matches('/');
    let path = if path == "/" { "" } else { path };
    let url = format!("{origin}/.well-known/oauth-authorization-server{path}");
    Url::parse(&url)
        .with_context(|| format!("invalid derived authorization-server metadata URL: {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_oauth::policy::OAuthClientFactory;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn server_config() -> zeroclaw_config::schema::McpServerConfig {
        zeroclaw_config::schema::McpServerConfig {
            name: "discovery-test".to_string(),
            // Mirrors production: a remote transport always carries the
            // configured URL, and it is the trust anchor for loopback/private
            // AS targets (every fetch here is against loopback mocks).
            url: Some("http://127.0.0.1:1/mcp".to_string()),
            ..Default::default()
        }
    }

    async fn mock_server() -> MockServer {
        let server = MockServer::start().await;
        // Every OAuth fetch in tests is against loopback http: exact-loopback
        // policy accepts it, and the IP literal skips DNS pinning.
        server
    }

    fn prm_body(resource: &str, issuers: &[String]) -> serde_json::Value {
        serde_json::json!({
            "resource": resource,
            "authorization_servers": issuers,
            "scopes_supported": ["openid"],
        })
    }

    fn asm_body(issuer: &str) -> serde_json::Value {
        serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "registration_endpoint": format!("{issuer}/register"),
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
        })
    }

    async fn full_flow_discovers_endpoints() {
        let prm = mock_server().await;
        let asm = mock_server().await;

        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(prm_body(&format!("{}/mcp", prm.uri()), &[asm.uri()])),
            )
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&asm.uri())))
            .mount(&asm)
            .await;

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical, None)
            .await
            .expect("discovery");
        assert_eq!(
            discovery.auth_server.authorization_endpoint.as_str(),
            format!("{}/authorize", asm.uri())
        );
        assert_eq!(
            discovery.auth_server.token_endpoint.as_str(),
            format!("{}/token", asm.uri())
        );
        assert_eq!(
            discovery
                .auth_server
                .registration_endpoint
                .as_ref()
                .map(|u| u.as_str()),
            Some(format!("{}/register", asm.uri()).as_str())
        );
        assert_eq!(
            discovery.protected_resource_url.as_str(),
            format!("{}/.well-known/oauth-protected-resource/mcp", prm.uri())
        );
    }

    #[tokio::test]
    async fn happy_path_challengeless_and_with_challenge() {
        full_flow_discovers_endpoints().await;
    }

    #[tokio::test]
    async fn challenge_resource_metadata_overrides_default_url() {
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/custom/prm-location"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(prm_body(&format!("{}/mcp", prm.uri()), &[asm.uri()])),
            )
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&asm.uri())))
            .mount(&asm)
            .await;

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let challenge = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/custom/prm-location""#,
            prm.uri()
        );
        let discovery = discover_oauth(&factory, &canonical, Some(&challenge))
            .await
            .expect("discovery via challenge hint");
        assert_eq!(
            discovery.protected_resource_url.as_str(),
            format!("{}/custom/prm-location", prm.uri())
        );
    }

    #[tokio::test]
    async fn path_issuers_probe_path_inserted_metadata_url() {
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                &format!("{}/mcp", prm.uri()),
                &[format!("{}/tenant", asm.uri())],
            )))
            .mount(&prm)
            .await;
        // RFC 8414 insertion: origin + well-known + issuer path.
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server/tenant"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(asm_body(&format!("{}/tenant", asm.uri()))),
            )
            .mount(&asm)
            .await;

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical, None)
            .await
            .expect("path issuer discovery");
        assert_eq!(
            discovery.auth_server.issuer.as_str(),
            format!("{}/tenant", asm.uri())
        );
    }

    #[tokio::test]
    async fn missing_prm_is_actionable_and_mentions_status() {
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&prm)
            .await;
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical, None)
            .await
            .expect_err("404 must fail discovery");
        assert!(err.to_string().contains("HTTP 404"), "got: {err}");
    }

    #[tokio::test]
    async fn all_issuer_failures_are_aggregated() {
        let prm = mock_server().await;
        let bad_asm = mock_server().await;
        let worse_asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                &format!("{}/mcp", prm.uri()),
                &[bad_asm.uri(), worse_asm.uri()],
            )))
            .mount(&prm)
            .await;
        // First issuer: metadata doc with a lying issuer.
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": "https://elsewhere.example",
                "authorization_endpoint": "https://elsewhere.example/a",
                "token_endpoint": "https://elsewhere.example/t",
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
            })))
            .mount(&bad_asm)
            .await;
        // Second issuer: not even JSON.
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html/>")
                    .insert_header("content-type", "text/html; charset=utf-8"),
            )
            .mount(&worse_asm)
            .await;

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical, None)
            .await
            .expect_err("all issuers bad");
        let text = err.to_string();
        assert!(
            text.contains("no advertised authorization server"),
            "got: {text}"
        );
        assert!(
            text.contains("does not match"),
            "first reason missing: {text}"
        );
        assert!(
            text.contains("content-type"),
            "second reason missing: {text}"
        );
    }

    #[tokio::test]
    async fn malformed_or_policy_violating_challenge_hint_falls_back_to_default() {
        // A hostile/loose server can advertise a resource_metadata that is a
        // policy violation (link-local literal) or plain garbage. Discovery
        // must fall back to the origin-bound default URL, not hard-fail and
        // not fetch the bad host.
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(prm_body(&format!("{}/mcp", prm.uri()), &[asm.uri()])),
            )
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&asm.uri())))
            .mount(&asm)
            .await;

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());

        for challenge in [
            r#"Bearer resource_metadata="http://169.254.169.254/.well-known/oauth-protected-resource/mcp""#,
            r#"Bearer resource_metadata="""#,
            r#"Bearer resource_metadata="/relative/only""#,
            r#"Bearer resource_metadata="ftp://host/prm""#,
        ] {
            let discovery = discover_oauth(&factory, &canonical, Some(challenge))
                .await
                .unwrap_or_else(|err| {
                    panic!("bad hint must fall back, not fail ({challenge}): {err}")
                });
            assert_eq!(
                discovery.protected_resource_url.as_str(),
                format!("{}/.well-known/oauth-protected-resource/mcp", prm.uri())
            );
        }
    }
}
