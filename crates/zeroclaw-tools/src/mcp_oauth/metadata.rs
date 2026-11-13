//! Bounded, content-typed parsing and validation of OAuth discovery
//! documents: RFC 9728 protected-resource metadata and RFC 8414
//! authorization-server metadata.
//!
//! Every fetch here is against server-controlled output, so the parsing layer
//! is deliberately paranoid: fixed size caps, `application/json` content-type
//! requirement, JSON-object requirement, and structural validation before any
//! endpoint is ever requested. Validation failures are actionable and contain
//! no credential material (these documents never carry credentials).

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Deserialize;

use super::policy::{MAX_METADATA_BYTES, ensure_oauth_url, origin_key};
use super::resource::CanonicalResource;

/// RFC 9728 protected-resource metadata (subset ZeroClaw consumes; unknown
/// members are ignored per the RFC).
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResourceMetadata {
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub authorization_servers: Option<Vec<String>>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    pub bearer_methods_supported: Option<Vec<String>>,
}

/// RFC 8414 authorization-server metadata (subset ZeroClaw consumes).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizationServerMetadata {
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub authorization_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub grant_types_supported: Option<Vec<String>>,
    #[serde(default)]
    pub response_types_supported: Option<Vec<String>>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// An authorization server whose metadata validated against the flow's
/// requirements: https/loopback posture, origin-pinned endpoints, PKCE S256,
/// the authorization-code grant, and public-client (`none`) token auth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAs {
    pub issuer: Url,
    pub authorization_endpoint: Url,
    pub token_endpoint: Url,
    /// Present only when advertised; DCR is still optional per configuration.
    pub registration_endpoint: Option<Url>,
    pub scopes_supported: Option<Vec<String>>,
}

/// GET one JSON document: no redirects (client posture), success status,
/// `application/json` content type, and a hard size bound.
pub async fn fetch_json_document(
    client: &reqwest::Client,
    url: &Url,
    what: &str,
) -> Result<serde_json::Value> {
    let response = client
        .get(url.as_str())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("failed to fetch {what} from {url}"))?;
    let status = response.status();
    // Defense-in-depth: even though the factory client disables redirects, a
    // caller could pass a permissive one; `response.url()` is the *final* URL
    // after any followed redirects, and 3xx here means the (unpinned) redirect
    // target answered. RFC 9728 §3.2 / 8414 §3.2 specify exactly 200 OK.
    if status.is_redirection() || response.url() != url {
        bail!(
            "{what} fetch from {url} was redirected (to {}); MCP OAuth metadata must not follow redirects",
            response.url()
        );
    }
    if status != reqwest::StatusCode::OK {
        bail!("{what} fetch from {url} returned HTTP {status}");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if media_type != "application/json" {
        bail!(
            "{what} fetch from {url} returned content-type {content_type:?}, expected application/json"
        );
    }
    let body =
        super::super::mcp_transport::read_body_bounded_to_string(response, MAX_METADATA_BYTES)
            .await
            .with_context(|| format!("{what} from {url} exceeded size or encoding limits"))?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("{what} from {url} is not valid JSON"))?;
    if !value.is_object() {
        bail!("{what} from {url} must be a JSON object");
    }
    Ok(value)
}

pub(crate) fn parse_document<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    what: &str,
) -> Result<T> {
    serde_json::from_value(value).with_context(|| format!("invalid {what} structure"))
}

/// Validate a protected-resource document against the canonical resource and
/// return its policy-checked authorization-server URLs.
pub fn validate_protected_resource(
    prm: ProtectedResourceMetadata,
    canonical: &CanonicalResource,
) -> Result<Vec<Url>> {
    match prm.resource.as_deref() {
        Some(resource) => {
            let parsed = Url::parse(resource).with_context(|| {
                format!("protected-resource metadata has unparseable resource {resource:?}")
            })?;
            let normalized = CanonicalResource::from_configured(&parsed)
                .with_context(|| format!("protected-resource metadata resource {resource:?} is not a canonicalizable http(s) URL"))?;
            if normalized.resource() != canonical.resource() {
                bail!(
                    "protected-resource metadata resource {:?} does not match the configured MCP resource {:?}",
                    normalized.resource(),
                    canonical.resource()
                );
            }
        }
        None => bail!("protected-resource metadata is missing the required `resource` member"),
    }
    if let Some(methods) = prm.bearer_methods_supported.as_deref()
        && !methods.iter().any(|m| m == "header")
    {
        bail!(
            "protected-resource metadata advertises bearer methods {methods:?}; ZeroClaw requires header bearer transmission"
        );
    }
    let servers = prm
        .authorization_servers
        .filter(|servers| !servers.is_empty())
        .with_context(|| {
            format!(
                "protected-resource metadata for {} lists no authorization servers",
                canonical.resource()
            )
        })?;
    let mut validated = Vec::with_capacity(servers.len());
    for server in servers {
        let url = Url::parse(&server)
            .with_context(|| format!("authorization server URL {server:?} is unparseable"))?;
        ensure_oauth_url(&url, "authorization server issuer")
            .with_context(|| format!("authorization server {server:?} violates the URL policy"))?;
        // RFC 8414 §2: issuer is a case-sensitive URL identifier with no
        // query or fragment components.
        if url.query().is_some() {
            bail!("authorization server issuer {server:?} must not carry a query component");
        }
        validated.push(url);
    }
    Ok(validated)
}

/// RFC 8414 §3.3 issuer comparison: exact scheme, case-insensitive host,
/// defaulted port, exact path. (The RFC's https relaxation is a subset of
/// this stricter equality after URL normalization.)
fn issuer_matches(actual: &Url, expected: &Url) -> bool {
    // RFC 8414 §2: an issuer identifier carries no query or fragment, so a
    // declared issuer that does carries none of the identity it claims;
    // rejecting it here keeps the check symmetric with the PRM side.
    if actual.query().is_some() || actual.fragment().is_some() {
        return false;
    }
    origin_key(actual).is_ok()
        && origin_key(expected).is_ok()
        && origin_key(actual).expect("checked") == origin_key(expected).expect("checked")
        && {
            let ap = actual.path().trim_end_matches('/');
            let ep = expected.path().trim_end_matches('/');
            let ap = if ap.is_empty() { "/" } else { ap };
            let ep = if ep.is_empty() { "/" } else { ep };
            ap == ep
        }
}

/// Validate an authorization-server document for the expected issuer and
/// return its usable endpoints.
pub fn validate_auth_server(document: serde_json::Value, issuer: &Url) -> Result<ValidatedAs> {
    let metadata: AuthorizationServerMetadata =
        parse_document::<AuthorizationServerMetadata>(document, "authorization-server metadata")?;

    let declared = metadata
        .issuer
        .as_deref()
        .context("authorization-server metadata is missing the required `issuer` member")?;
    let declared = Url::parse(declared).with_context(|| {
        format!("authorization-server metadata issuer {declared:?} is unparseable")
    })?;
    if !issuer_matches(&declared, issuer) {
        bail!(
            "authorization-server metadata issuer {:?} does not match the protected-resource declaration {issuer}",
            declared.as_str()
        );
    }

    let endpoint = |field: &str, raw: Option<&String>| -> Result<Url> {
        let raw =
            raw.with_context(|| format!("authorization-server metadata is missing `{field}`"))?;
        let url = Url::parse(raw).with_context(|| {
            format!("authorization-server metadata {field} {raw:?} is unparseable")
        })?;
        ensure_oauth_url(&url, field).with_context(|| {
            format!("authorization-server metadata {field} {raw:?} violates the URL policy")
        })?;
        if origin_key(&url)? != origin_key(issuer)? {
            bail!(
                "authorization-server metadata {field} {:?} must share the issuer origin {issuer}",
                url.as_str()
            );
        }
        Ok(url)
    };
    let authorization_endpoint = endpoint(
        "authorization_endpoint",
        metadata.authorization_endpoint.as_ref(),
    )?;
    let token_endpoint = endpoint("token_endpoint", metadata.token_endpoint.as_ref())?;
    let registration_endpoint = metadata
        .registration_endpoint
        .as_ref()
        .map(|_| {
            endpoint(
                "registration_endpoint",
                metadata.registration_endpoint.as_ref(),
            )
        })
        .transpose()?;

    // Capability gates for the one grant ZeroClaw implements. Absent lists
    // fall back to the RFC defaults, which never satisfy public-client PKCE.
    let grants = metadata
        .grant_types_supported
        .unwrap_or_else(|| vec!["authorization_code".to_string(), "implicit".to_string()]);
    if !grants.iter().any(|g| g == "authorization_code") {
        bail!(
            "authorization server {issuer} does not support the authorization_code grant (advertiseds: {grants:?})"
        );
    }
    let responses = metadata
        .response_types_supported
        .unwrap_or_else(|| vec!["code".to_string()]);
    if !responses.iter().any(|r| r == "code") {
        bail!(
            "authorization server {issuer} does not support the `code` response type (advertiseds: {responses:?})"
        );
    }
    let challenges = metadata
        .code_challenge_methods_supported
        .unwrap_or_default();
    if !challenges.iter().any(|m| m == "S256") {
        bail!(
            "authorization server {issuer} does not advertise PKCE S256 support (advertiseds: {challenges:?}); plain authorization-code without PKCE is not accepted for public clients"
        );
    }
    let auth_methods = metadata
        .token_endpoint_auth_methods_supported
        .unwrap_or_else(|| vec!["client_secret_basic".to_string()]);
    if !auth_methods.iter().any(|m| m == "none") {
        bail!(
            "authorization server {issuer} does not support public-client token auth (`none`); ZeroClaw does not manage client secrets (advertiseds: {auth_methods:?})"
        );
    }

    Ok(ValidatedAs {
        issuer: issuer.clone(),
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        scopes_supported: metadata.scopes_supported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(raw: &str) -> CanonicalResource {
        CanonicalResource::from_configured(&Url::parse(raw).expect("fixture URL parses"))
            .expect("canonical")
    }

    fn prm_json(resource: &str, servers: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "resource": resource,
            "authorization_servers": servers,
        })
    }

    #[test]
    fn prm_binding_requires_exact_canonical_match() {
        let canonical = canonical("https://mcp.silpo.ua/mcp");
        let ok = validate_protected_resource(
            parse_document(
                serde_json::json!({
                    "resource": "https://mcp.silpo.ua/mcp/",
                    "authorization_servers": ["https://mcp.silpo.ua"],
                }),
                "prm",
            )
            .expect("structure"),
            &canonical,
        )
        .expect("matching resource validates");
        assert_eq!(ok[0].as_str(), "https://mcp.silpo.ua/");

        let err = validate_protected_resource(
            parse_document(
                prm_json("https://evil.example/mcp", &["https://mcp.silpo.ua"]),
                "prm",
            )
            .expect("structure"),
            &canonical,
        )
        .expect_err("cross-resource mix-up must fail closed");
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }

    #[test]
    fn prm_requires_resource_servers_and_header_bearer() {
        let canonical = canonical("https://h/mcp");
        let err = validate_protected_resource(
            parse_document(
                serde_json::json!({"authorization_servers": ["https://a"]}),
                "prm",
            )
            .expect("structure"),
            &canonical,
        )
        .expect_err("missing resource");
        assert!(
            err.to_string().contains("missing the required `resource`"),
            "got: {err}"
        );

        let err = validate_protected_resource(
            parse_document(prm_json("https://h/mcp", &[]), "prm").expect("structure"),
            &canonical,
        )
        .expect_err("empty AS list");
        assert!(
            err.to_string().contains("no authorization servers"),
            "got: {err}"
        );

        let err = validate_protected_resource(
            parse_document(
                serde_json::json!({
                    "resource": "https://h/mcp",
                    "authorization_servers": ["https://a"],
                    "bearer_methods_supported": ["body"],
                }),
                "prm",
            )
            .expect("structure"),
            &canonical,
        )
        .expect_err("non-header bearer method");
        assert!(err.to_string().contains("header bearer"), "got: {err}");
    }

    #[test]
    fn prm_authorization_servers_follow_url_policy() {
        let canonical = canonical("https://h/mcp");
        let err = validate_protected_resource(
            parse_document(prm_json("https://h/mcp", &["http://meta.example"]), "prm")
                .expect("structure"),
            &canonical,
        )
        .expect_err("plaintext non-loopback issuer");
        assert!(err.to_string().contains("URL policy"), "got: {err}");
    }

    fn asm_json(overrides: serde_json::Value) -> serde_json::Value {
        let mut base = serde_json::json!({
            "issuer": "https://auth.example",
            "authorization_endpoint": "https://auth.example/authorize",
            "token_endpoint": "https://auth.example/token",
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
        });
        if let (Some(base_obj), Some(over_obj)) = (base.as_object_mut(), overrides.as_object()) {
            for (key, value) in over_obj {
                if value.is_null() {
                    base_obj.remove(key);
                } else {
                    base_obj.insert(key.clone(), value.clone());
                }
            }
        }
        base
    }

    fn issuer() -> Url {
        Url::parse("https://auth.example").expect("fixture")
    }

    #[test]
    fn asm_happy_path_yields_usable_endpoints() {
        let validated =
            validate_auth_server(asm_json(serde_json::json!({})), &issuer()).expect("valid AS");
        assert_eq!(
            validated.token_endpoint.as_str(),
            "https://auth.example/token"
        );
        assert!(validated.registration_endpoint.is_none());
    }

    #[test]
    fn asm_issuer_mismatch_and_path_rules() {
        let err = validate_auth_server(
            asm_json(serde_json::json!({"issuer": "https://evil.example"})),
            &issuer(),
        )
        .expect_err("issuer mix-up guard");
        assert!(err.to_string().contains("does not match"), "got: {err}");

        // Path-bearing issuers must match exactly; sibling paths do not count.
        let path_issuer = Url::parse("https://auth.example/tenant1").expect("fixture");
        validate_auth_server(
            asm_json(serde_json::json!({
                "issuer": "https://auth.example/tenant1",
                "authorization_endpoint": "https://auth.example/tenant1/authorize",
                "token_endpoint": "https://auth.example/tenant1/token",
            })),
            &path_issuer,
        )
        .expect("exact path issuer ok");
        validate_auth_server(
            asm_json(serde_json::json!({
                "issuer": "https://auth.example/tenant2",
                "authorization_endpoint": "https://auth.example/tenant2/authorize",
                "token_endpoint": "https://auth.example/tenant2/token",
            })),
            &path_issuer,
        )
        .expect_err("sibling path rejected");
    }

    #[test]
    fn asm_endpoints_must_share_issuer_origin_and_policy() {
        let err = validate_auth_server(
            asm_json(serde_json::json!({"token_endpoint": "https://elsewhere.example/token"})),
            &issuer(),
        )
        .expect_err("cross-origin token endpoint");
        assert!(
            err.to_string().contains("share the issuer origin"),
            "got: {err}"
        );

        let err = validate_auth_server(
            asm_json(
                serde_json::json!({"authorization_endpoint": "http://auth.example/authorize"}),
            ),
            &issuer(),
        )
        .expect_err("plaintext endpoint");
        assert!(err.to_string().contains("URL policy"), "got: {err}");
    }

    #[test]
    fn asm_capability_gates_fail_closed() {
        for overrides in [
            serde_json::json!({"grant_types_supported": ["implicit"]}),
            serde_json::json!({"response_types_supported": ["token"]}),
            serde_json::json!({"code_challenge_methods_supported": ["plain"]}),
            serde_json::json!({"token_endpoint_auth_methods_supported": ["client_secret_basic"]}),
            // Absent PKCE/auth-method lists default to configurations that
            // cannot satisfy public-client PKCE and must not pass.
            serde_json::json!({"code_challenge_methods_supported": null}),
            serde_json::json!({"token_endpoint_auth_methods_supported": null}),
        ] {
            let err = validate_auth_server(asm_json(overrides.clone()), &issuer())
                .expect_err("capability gate must reject {overrides}");
            assert!(
                err.to_string().contains("authorization server")
                    || err.to_string().contains("does not support"),
                "got: {err}"
            );
        }
    }

    #[test]
    fn missing_required_asm_members_are_actionable() {
        let err = validate_auth_server(
            asm_json(serde_json::json!({"token_endpoint": null})),
            &issuer(),
        )
        .expect_err("missing token_endpoint");
        assert!(err.to_string().contains("`token_endpoint`"), "got: {err}");
    }

    #[test]
    fn issuer_with_query_component_is_rejected() {
        // RFC 8414 §2: the issuer identifier has no query or fragment.
        let canonical = canonical("https://h/mcp");
        let err = validate_protected_resource(
            parse_document(
                prm_json("https://h/mcp", &["https://auth.example/?tenant=1"]),
                "prm",
            )
            .expect("structure"),
            &canonical,
        )
        .expect_err("issuer query");
        assert!(err.to_string().contains("query component"), "got: {err}");
    }

    #[test]
    fn declared_issuer_with_query_is_rejected_as_mismatch() {
        // Symmetry with the PRM side: an `issuer` member carrying a query or
        // fragment claims no identity and must not match, even when
        // origin+path would otherwise agree.
        let err = validate_auth_server(
            asm_json(serde_json::json!({"issuer": "https://auth.example/?x=1"})),
            &issuer(),
        )
        .expect_err("declared issuer query breaks identity");
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }

    #[test]
    fn absent_response_types_defaults_to_code_leniently() {
        // RFC 8414 §5 default for response_types_supported is ["code"], which
        // satisfies the flow; a server that omits the member must still pass.
        let validated = validate_auth_server(
            asm_json(serde_json::json!({"response_types_supported": null})),
            &issuer(),
        )
        .expect("absent response_types must default to code");
        assert_eq!(
            validated.token_endpoint.as_str(),
            "https://auth.example/token"
        );
    }

    #[tokio::test]
    async fn fetch_enforces_status_content_type_and_size() {
        // Hermetic: a tiny in-crate hyper server avoids network DNS.
        use std::net::SocketAddr;
        async fn serve(
            body: &'static str,
            content_type: &'static str,
            status: u16,
        ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let handle = zeroclaw_spawn::spawn!(async move {
                let (mut stream, _) = listener.accept().await.expect("accept");
                use tokio::io::AsyncWriteExt;
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
            (addr, handle)
        }

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        // 200 + application/json passes.
        let (addr, handle) = serve(r#"{"ok":true}"#, "application/json;charset=utf-8", 200).await;
        let value = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect("valid document");
        assert_eq!(value["ok"], true);
        handle.abort();

        // 404 fails.
        let (addr, handle) = serve("{}", "application/json", 404).await;
        let err = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect_err("404");
        assert!(err.to_string().contains("HTTP 404"), "got: {err}");
        handle.abort();

        // 3xx must fail as a redirect, never as a generic status.
        let (addr, handle) = serve("{}", "application/json", 302).await;
        let err = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect_err("3xx must be rejected");
        assert!(err.to_string().contains("redirected"), "got: {err}");
        handle.abort();

        // Only exact 200 is accepted (RFC 9728 §3.2 / 8414 §3.2).
        let (addr, handle) = serve("", "application/json", 204).await;
        let err = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect_err("204 must be rejected");
        assert!(err.to_string().contains("HTTP 204"), "got: {err}");
        handle.abort();

        // text/html fails.
        let (addr, handle) = serve("<html/>", "text/html", 200).await;
        let err = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect_err("content type");
        assert!(err.to_string().contains("content-type"), "got: {err}");
        handle.abort();

        // Oversized body fails via the streaming cap (no content-length).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let handle = zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            use tokio::io::AsyncWriteExt;
            let _ = stream
                .write_all(b"HTTP/1.1 200 X\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n")
                .await;
            let chunk = "a".repeat(16 * 1024);
            for _ in 0..8 {
                if stream
                    .write_all(format!("{:X}\r\n{chunk}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // Stay open so the reader keeps consuming until the cap trips.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        });
        let err = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect_err("oversized");
        assert!(
            err.to_string().contains("size or encoding limits"),
            "got: {err}"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn documents_reject_non_object_and_bad_shapes() {
        use std::net::SocketAddr;
        async fn serve_json(body: &'static str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let handle = zeroclaw_spawn::spawn!(async move {
                let (mut stream, _) = listener.accept().await.expect("accept");
                use tokio::io::AsyncWriteExt;
                let response = format!(
                    "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
            (addr, handle)
        }
        let client = reqwest::Client::new();
        let (addr, handle) = serve_json("[]").await;
        let err = fetch_json_document(
            &client,
            &Url::parse(&format!("http://{addr}/doc")).expect("url"),
            "probe",
        )
        .await
        .expect_err("non-object JSON");
        assert!(
            err.to_string().contains("must be a JSON object"),
            "got: {err}"
        );
        handle.abort();

        let err = parse_document::<ProtectedResourceMetadata>(
            serde_json::json!({"authorization_servers": "not-a-list"}),
            "prm",
        )
        .expect_err("bad shape");
        assert!(
            err.to_string().contains("invalid prm structure"),
            "got: {err}"
        );
    }
}
