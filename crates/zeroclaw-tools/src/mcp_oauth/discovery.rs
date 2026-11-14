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

use super::challenge::{parse_bearer_challenge, pick_bearer_challenge};
use super::metadata::{
    ProtectedResourceMetadata, ValidatedAs, fetch_json_document, fetch_json_document_opt,
    parse_document, validate_auth_server, validate_protected_resource,
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

/// Wall-clock budget for ONE discovery pass. The transport deliberately
/// runs `prepare_auth` without a request timeout (an interactive sign-in
/// may take minutes), and `connect_all` is sequential, so unbounded
/// discovery would let one pathological server stall every later connect:
/// candidate count is also bounded below, but the wall clock is the
/// backstop. Sign-in time lives in `authorize`, AFTER discovery, and is
/// not covered by this budget.
pub(crate) const DISCOVERY_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);
/// Per-issuers probed per resource in one pass; the remainder is named in
/// the aggregate error instead of fetched (a PRM listing more than this is
/// either broken or hostile).
const MAX_ISSUERS: usize = 4;

/// Extract a USABLE `resource_metadata` hint from a raw `WWW-Authenticate`
/// header: parsed, absolute, and passing the OAuth URL policy. The second
/// field is a human-readable rejection reason when a hint was present but
/// unusable, so the caller can record WHY the server's own advertisement
/// was not followed (SSRF rail: hostile hints are named, never fetched).
pub(crate) fn usable_challenge_hint(raw: &str) -> (Option<Url>, Option<String>) {
    let Some(challenge) = parse_bearer_challenge(raw) else {
        return (None, None);
    };
    let Some(metadata) = challenge.resource_metadata else {
        return (None, None);
    };
    if metadata.trim().is_empty() {
        return (
            None,
            Some("challenge advertised an empty resource_metadata".to_string()),
        );
    }
    let parsed = (|| -> Result<Url> {
        let url = Url::parse(&metadata)?;
        ensure_oauth_url(&url, "protected-resource metadata")?;
        Ok(url)
    })();
    match parsed {
        Ok(url) => (Some(url), None),
        Err(reason) => (
            None,
            Some(format!(
                "challenge hint {:?} rejected by the URL policy: {reason}",
                super::truncate_untrusted(&metadata)
            )),
        ),
    }
}

/// RFC 9728 defines ONE well-known URL (the path form). The MCP
/// authorization spec turns discovery into a contract for MCP clients:
/// challenge hint if present, then the path form, then the ROOT form
/// `/.well-known/oauth-protected-resource` without the path suffix. This is
/// that third candidate.
fn root_prm_url(canonical: &CanonicalResource) -> Result<Url> {
    Url::parse(&format!(
        "{}/.well-known/oauth-protected-resource",
        canonical.origin_prefix()
    ))
    .context("cannot derive the root protected-resource metadata URL")
}

/// Outcome of one protected-resource document attempt. A document that
/// MISSES (absent, redirected, wrong content-type, unreachable origin) is a
/// string reason so the candidate chain continues; a document that EXISTS
/// but breaks the contract (unparseable, wrong `resource`, no authorization
/// servers) is INVALID: the server is asserting metadata and getting it
/// wrong, and the chain must not paper over that by trying another
/// location. A refused client (host pinned away, disallowed scheme) is a
/// miss with the policy phrase already rendered.
enum Prm {
    Advertised(Vec<Url>),
    Miss(String),
    Invalid(String),
}

async fn try_prm(factory: &OAuthClientFactory, canonical: &CanonicalResource, url: &Url) -> Prm {
    let client = match factory.client_for(url).await {
        Ok(client) => client,
        Err(err) => return Prm::Miss(format!("GET {url} → client refused: {err:#}")),
    };
    let document = match fetch_json_document_opt(&client, url, "protected-resource metadata").await
    {
        Ok(super::metadata::DocFetch::Found(document)) => document,
        Ok(super::metadata::DocFetch::Miss(reason)) => return Prm::Miss(reason),
        Err(err) => return Prm::Miss(format!("{err:#}")),
    };
    match parse_document::<ProtectedResourceMetadata>(document, "protected-resource metadata")
        .and_then(|prm| validate_protected_resource(prm, canonical))
    {
        Ok(issuers) => Prm::Advertised(issuers),
        Err(err) => Prm::Invalid(format!("{url} → {err:#}")),
    }
}

/// Enumerate `authorization_servers` from RFC 9728 §3; the MCP spec's MUST
/// form list for one issuer identifier, deduplicated: the RFC 8414
/// origin-inserted well-known, the OIDC origin-inserted well-known, and
/// (path issuers only) the OIDC path-appended form. RFC 8414 §5 documents that
/// path-appended RFC 8414 URLs are NOT standard (the appended form belongs
/// to OIDC Discovery §4.1), so it is tried only where the OIDC spec puts
/// it, last.
fn as_metadata_candidates(issuer: &Url) -> Result<Vec<Url>> {
    let origin = origin_key(issuer).context("authorization server issuer has no origin")?;
    let path = issuer.path().trim_end_matches('/');
    let path = if path == "/" { "" } else { path };
    let mut candidates = vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}/.well-known/openid-configuration{path}"),
    ];
    if !path.is_empty() {
        let base = issuer.as_str().trim_end_matches('/');
        candidates.push(format!("{base}/.well-known/openid-configuration"));
    }
    candidates
        .into_iter()
        .map(|raw| Url::parse(&raw).with_context(|| format!("invalid derived metadata URL: {raw}")))
        .collect()
}

/// Run discovery for `canonical`. `www_authenticate` is the raw
/// `WWW-Authenticate` header from the server's 401, when one was captured;
/// its `resource_metadata` parameter is the FIRST protected-resource
/// document candidate (policy-checked; a hostile hint is named and never
/// fetched), then the RFC 9728 path form, then the root well-known form.
/// If every document candidate MISSED, one anonymous probe GET of the
/// configured endpoint runs (the MCP-spec challenge source), followed by one
/// anonymous `initialize` POST when that endpoint answers 405: a 401/403
/// hint re-enters the document chain, and an origin that challenged but
/// publishes no PRM at all gets one last, tightly pinned fallback: AS
/// metadata on the resource's own origin, accepted only when the document
/// declares that origin as its issuer.
///
/// Every fetch uses a per-target client from `factory`, so each
/// server-supplied URL gets its own host validation and DNS pinning; the
/// factory must not be replaced by one shared client. Candidate URLs are
/// deduplicated before any request, issuers are capped at `MAX_ISSUERS`,
/// and the whole pass is meant to run inside `DISCOVERY_BUDGET`.
pub async fn discover_oauth(
    factory: &OAuthClientFactory,
    canonical: &CanonicalResource,
    www_authenticate: Option<&str>,
) -> Result<OAuthDiscovery> {
    let mut trail: Vec<String> = Vec::new();

    let hint = www_authenticate.map(|raw| {
        let (url, rejection) = usable_challenge_hint(raw);
        if let Some(reason) = rejection {
            trail.push(reason);
        }
        url
    });
    let default_prm_url = canonical
        .well_known_prm_url()
        .context("cannot derive the protected-resource metadata URL")?;

    // Candidate chain in MCP-spec order, deduplicated: challenge hint →
    // path well-known → root well-known (a root-path resource folds these
    // last two together, so the dedup keeps one request).
    let mut prm: Option<(Url, Vec<Url>)> = None;
    let mut hard_failure: Option<String> = None;
    let mut tried: Vec<Url> = Vec::new();
    for url in dedup_urls(vec![
        hint.flatten(),
        Some(default_prm_url.clone()),
        Some(root_prm_url(canonical)?),
    ]) {
        tried.push(url.clone());
        match try_prm(factory, canonical, &url).await {
            Prm::Advertised(issuers) => {
                prm = Some((url, issuers));
                break;
            }
            Prm::Miss(reason) => trail.push(reason),
            Prm::Invalid(reason) => {
                trail.push(reason);
                hard_failure = Some(
                    "a protected-resource document exists but violates the contract".to_string(),
                );
                break;
            }
        }
    }

    // The probe is the LAST PRM-location candidate: a healthy server is
    // answered by the chain above and never pays for it.
    let mut challenge_attested = false;
    if prm.is_none() && hard_failure.is_none() {
        match probe_resource(factory, canonical).await {
            Ok(Probe::Challenged(probe_hint, rejection)) => {
                challenge_attested = true;
                if let Some(reason) = rejection {
                    trail.push(format!("probe challenge: {reason}"));
                } else {
                    trail.push(
                        "probe of the configured endpoint returned a bearer challenge".to_string(),
                    );
                }
                if let Some(url) = probe_hint {
                    if tried.contains(&url) {
                        trail.push(format!(
                            "probe hint {url} names the location already tried; not fetched again"
                        ));
                    } else {
                        tried.push(url.clone());
                        match try_prm(factory, canonical, &url).await {
                            Prm::Advertised(issuers) => prm = Some((url, issuers)),
                            Prm::Miss(reason) => trail.push(reason),
                            Prm::Invalid(reason) => {
                                // Same rule as the well-known chain: a
                                // document that exists and lies is a hard
                                // failure, not a Miss to rescue over.
                                trail.push(reason);
                                hard_failure = Some(
                                    "a protected-resource document exists but violates the contract"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
            }
            Ok(Probe::Rejected(status)) => {
                // The origin spoke, but a 404 off a misconfigured path or a
                // protocol-first 400 says nothing about protection, so this
                // does NOT unlock the origin-pinned AS fallback below: only a
                // bearer challenge attests an authorization layer.
                trail.push(format!(
                    "probe of the configured endpoint returned HTTP {status}, attesting the origin answered but not that an authorization layer exists"
                ));
            }
            Ok(Probe::Answered(status)) => {
                trail.push(format!(
                    "probe of the configured endpoint returned HTTP {status} without a bearer challenge"
                ));
            }
            // Connect/timeout/policy failure: the origin itself is
            // unreachable, so origin-pinned candidates are pointless.
            Err(err) => trail.push(format!("probe of the configured endpoint failed: {err:#}")),
        }
    }

    if let Some((prm_url, issuers)) = prm {
        // Try each advertised authorization server; the first fully valid
        // metadata document wins. Failure reasons are accumulated, never
        // short-circuited, so the final error explains every candidate.
        let mut failures = Vec::new();
        for issuer in issuers.iter().take(MAX_ISSUERS) {
            let candidates = match as_metadata_candidates(issuer) {
                Ok(candidates) => dedup_urls(candidates.into_iter().map(Some).collect()),
                Err(err) => {
                    failures.push(format!("{issuer}: {err:#}"));
                    continue;
                }
            };
            let mut issuer_failures = Vec::new();
            for url in candidates {
                match fetch_auth_server(factory, &url, issuer).await {
                    Ok(auth_server) => {
                        return Ok(OAuthDiscovery {
                            canonical: canonical.clone(),
                            protected_resource_url: prm_url,
                            auth_server,
                        });
                    }
                    Err(err) => issuer_failures.push(format!("{url} → {err:#}")),
                }
            }
            failures.push(format!(
                "{issuer}: {}",
                truncate_per_candidate(&issuer_failures)
            ));
        }
        if issuers.len() > MAX_ISSUERS {
            let skipped: Vec<&str> = issuers.iter().skip(MAX_ISSUERS).map(Url::as_str).collect();
            failures.push(format!(
                "not tried (issuer cap {MAX_ISSUERS}): {}",
                super::truncate_untrusted(&skipped.join(", "))
            ));
        }
        bail!(
            "no advertised authorization server produced valid metadata for {} — {}",
            canonical.resource(),
            trail
                .iter()
                .chain(failures.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    // No usable PRM document anywhere, but the anonymous probe drew a 401/403
    // bearer challenge: the endpoint says it needs a bearer while publishing
    // no PRM, and the MCP spec allows "abort or use pre-configured values".
    // An error status that is not a challenge (a 404 off a misconfigured
    // path, a protocol-first 400) attests only that the origin answered, so
    // it does not unlock this branch. The pre-configured AS is the OPERATOR's
    // own origin, pinned hard: the document must declare exactly that origin
    // as its issuer, so this can only ever discover "this same host is its
    // own authorization server" (the legacy same-origin shape), never an
    // off-site AS the PRM would have had to vouch for.
    if challenge_attested && hard_failure.is_none() {
        let origin = Url::parse(&canonical.origin_prefix())
            .context("canonical resource origin must re-parse as a URL")?;
        for url in dedup_urls(vec![
            Some(
                Url::parse(&format!(
                    "{}/.well-known/oauth-authorization-server",
                    canonical.origin_prefix()
                ))
                .context("cannot derive the origin authorization-server URL")?,
            ),
            Some(
                Url::parse(&format!(
                    "{}/.well-known/openid-configuration",
                    canonical.origin_prefix()
                ))
                .context("cannot derive the origin OpenID configuration URL")?,
            ),
        ]) {
            match fetch_auth_server(factory, &url, &origin).await {
                Ok(auth_server) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "mcp_oauth": "origin-as-fallback",
                                "resource": canonical.resource(),
                                "issuer": auth_server.issuer.as_str(),
                            })),
                        &format!(
                            "MCP OAuth found no protected-resource metadata for {}; \
                             using the resource origin's own authorization server {}",
                            canonical.resource(),
                            auth_server.issuer
                        )
                    );
                    return Ok(OAuthDiscovery {
                        canonical: canonical.clone(),
                        // No protected-resource document backed this
                        // result: record the document that did.
                        protected_resource_url: url,
                        auth_server,
                    });
                }
                Err(err) => trail.push(format!("{url} → {err:#}")),
            }
        }
    }

    let hint_line = "hint: publish protected-resource metadata at the path or root well-known URL, or advertise resource_metadata in the 401";
    if hard_failure.is_some() {
        bail!(
            "protected-resource metadata for {} is present but invalid — {} ; {}",
            canonical.resource(),
            trail.join("; "),
            hint_line
        );
    }
    bail!(
        "no usable protected-resource metadata for {} — {} ; {}",
        canonical.resource(),
        trail.join("; "),
        hint_line
    )
}

/// Drop `None` and duplicate URLs, preserving order.
fn dedup_urls(urls: Vec<Option<Url>>) -> Vec<Url> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for url in urls.into_iter().flatten() {
        if !seen.contains(&url) {
            seen.push(url.clone());
            out.push(url);
        }
    }
    out
}

/// Per-candidate reasons are each kept short (they embed full URLs) so
/// every candidate stays named in the aggregate error, which ends up in a
/// single connect log line.
fn truncate_per_candidate(reasons: &[String]) -> String {
    reasons
        .iter()
        .map(|reason| super::truncate_untrusted(reason))
        .collect::<Vec<_>>()
        .join("; ")
}

enum Probe {
    /// 401/403: the endpoint answered the anonymous request with a bearer
    /// challenge. The payload is a policy-usable `resource_metadata` hint
    /// when the challenge carried one; the second field carries WHY an
    /// advertised hint the URL policy refused.
    Challenged(Option<Url>, Option<String>),
    /// Other 4xx/5xx: the origin answered with an error. That attests the
    /// origin is reachable (so origin-pinned candidates are worth a shot)
    /// but does NOT attest that an authorization layer exists.
    Rejected(reqwest::StatusCode),
    /// The endpoint answered non-error WITHOUT a bearer challenge: the
    /// server is not asking for OAuth, so fallback chains that assume an
    /// authorization layer must not fire.
    Answered(reqwest::StatusCode),
}

/// Anonymous GET of the operator-configured endpoint asking for the
/// challenge the MCP spec promises on unauthorized transport requests.
/// Safe method, no session headers (auth middleware typically answers
/// before protocol validation), `Accept` mirrors the transport so
/// streamable-HTTP servers MAY classify it as a transport request rather
/// than 400-ing a browser-style GET, body never read, and NO Authorization
/// ever: the probe reports what the server says about anonymous access,
/// and cannot leak a bearer. A 405 GET answer is followed by one anonymous
/// `initialize` POST, because a streamable-HTTP endpoint that declines GET
/// answers the same question over POST and may only emit the challenge
/// there; any session that answer opens is discarded with the response.
async fn probe_resource(
    factory: &OAuthClientFactory,
    canonical: &CanonicalResource,
) -> Result<Probe> {
    let url = canonical.configured();
    let client = factory.client_for(url).await?;
    let response = client
        .get(url.as_str())
        .header("accept", "application/json, text/event-stream")
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .map_err(anyhow::Error::msg)?;
    if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        let response = client
            .post(url.as_str())
            .header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": crate::mcp_protocol::MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "zeroclaw",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                },
            }))
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .map_err(anyhow::Error::msg)?;
        return Ok(classify_probe(response));
    }
    Ok(classify_probe(response))
}

/// Turn one anonymous probe answer into a [`Probe`]: a 401/403 challenge
/// (with its policy-usable `resource_metadata` hint), any other error
/// status, or a non-error answer.
fn classify_probe(response: reqwest::Response) -> Probe {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        let (hint, rejection) = pick_bearer_challenge(response.headers())
            .as_deref()
            .map(usable_challenge_hint)
            .unwrap_or((None, None));
        return Probe::Challenged(hint, rejection);
    }
    if status.is_client_error() || status.is_server_error() {
        // 404 and friends: the origin answered with an error. A
        // protocol-first server may reject an unsessioned request on
        // protocol grounds; either way the origin spoke, but no challenge
        // exists.
        return Probe::Rejected(status);
    }
    Probe::Answered(status)
}

/// Fetch and validate AS metadata for one candidate URL of one issuer.
async fn fetch_auth_server(
    factory: &OAuthClientFactory,
    metadata_url: &Url,
    issuer: &Url,
) -> Result<ValidatedAs> {
    let client = factory
        .client_for(metadata_url)
        .await
        .with_context(|| format!("cannot build a client for {metadata_url}"))?;
    let document = fetch_json_document(&client, metadata_url, "authorization-server metadata")
        .await
        .map_err(|err| {
            // Keep the candidate's reason short: it is joined into the
            // aggregate discovery error.
            anyhow::Error::msg(format!("{err:#}"))
        })?;
    validate_auth_server(document, issuer)
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

    // ---- PRM candidate chain: probe, root form, hard failures ----------

    fn canonical_for(prm: &MockServer) -> CanonicalResource {
        CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical")
    }

    #[tokio::test]
    async fn probe_recovers_prm_advertised_only_in_challenge() {
        // The server publishes NO well-known PRM (path and root both 404)
        // but advertises `resource_metadata` in its 401 challenge: the
        // probe must surface that hint and discovery must follow it.
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer realm="mcp", resource_metadata="{}/custom/prm""#,
                    prm.uri()
                ),
            ))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/custom/prm"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(prm_body(&format!("{}/mcp?sid=9", prm.uri()), &[asm.uri()])),
            )
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&asm.uri())))
            .mount(&asm)
            .await;

        // A session-carrying configured URL must reach the probe verbatim:
        // routing-sensitive queries would otherwise probe the wrong vhost.
        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp?sid=9", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical, None)
            .await
            .expect("probe hint must recover discovery");
        assert_eq!(
            discovery.auth_server.authorization_endpoint.as_str(),
            format!("{}/authorize", asm.uri())
        );
        let requests = prm.received_requests().await.expect("recorded");
        let get = |p: &str| {
            requests
                .iter()
                .filter(|r| r.url.path() == p)
                .collect::<Vec<_>>()
        };
        // The probe is anonymous: it must never carry a bearer.
        for probe in get("/mcp") {
            assert!(
                !probe.headers.contains_key("authorization"),
                "probe carried a bearer"
            );
            assert_eq!(
                probe.url.query(),
                Some("sid=9"),
                "probe must keep the query"
            );
            let accept = probe
                .headers
                .get("accept")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            assert!(
                accept.contains("application/json") && accept.contains("text/event-stream"),
                "probe must present the MCP transport Accept set, got {accept:?}"
            );
        }
        assert_eq!(get("/mcp").len(), 1, "probe fired exactly once");
        assert_eq!(get("/custom/prm").len(), 1);
        assert_eq!(get("/.well-known/oauth-protected-resource/mcp").len(), 1);
        assert_eq!(get("/.well-known/oauth-protected-resource").len(), 1);
    }

    #[tokio::test]
    async fn probe_recovers_prm_advertised_behind_a_separate_basic_challenge() {
        // One field per advertised scheme is legal (RFC 9110 §8.6.1), and a
        // gateway in front of the login service commonly answers `Basic`
        // first: reading only the first `WWW-Authenticate` field drops the
        // Bearer parameters, and the only place naming the login service is
        // then lost before `initialize` ever runs.
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(401)
                    .insert_header("www-authenticate", "Basic realm=\"mcp\"")
                    .append_header(
                        "www-authenticate",
                        format!(
                            r#"Bearer realm="mcp", resource_metadata="{}/custom/prm""#,
                            prm.uri()
                        ),
                    ),
            )
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/custom/prm"))
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

        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect("a challenge in a later field must still drive discovery");
        assert_eq!(
            discovery.auth_server.authorization_endpoint.as_str(),
            format!("{}/authorize", asm.uri())
        );
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/custom/prm")
                .count(),
            1,
            "the hint carried by the later field is the one discovery followed"
        );
    }

    #[tokio::test]
    async fn post_only_challenge_is_discovered_when_get_is_refused_with_405() {
        // A streamable-HTTP gateway that declines GET with 405 answers the
        // same anonymous question over POST, and some of them only emit the
        // `resource_metadata` challenge there. The probe must follow the
        // 405 with one `initialize` POST and use the challenge it returns;
        // otherwise `prepare_auth` fails before `initialize` ever runs.
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&prm)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer realm="mcp", resource_metadata="{}/custom/prm""#,
                    prm.uri()
                ),
            ))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/custom/prm"))
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
            .expect("the POST challenge must recover discovery");
        assert_eq!(
            discovery.protected_resource_url.as_str(),
            format!("{}/custom/prm", prm.uri()),
            "the document must come from the POST challenge hint"
        );
        assert_eq!(
            discovery.auth_server.authorization_endpoint.as_str(),
            format!("{}/authorize", asm.uri())
        );

        let requests = prm.received_requests().await.expect("recorded");
        let posts = requests
            .iter()
            .filter(|r| r.url.path() == "/mcp" && r.method.as_str() == "POST")
            .collect::<Vec<_>>();
        assert_eq!(posts.len(), 1, "the 405 must be followed by one POST probe");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/mcp" && r.method.as_str() == "GET")
                .count(),
            1,
            "the GET probe fires once, the POST only as its 405 follow-up"
        );
        for probe in &posts {
            assert!(
                !probe.headers.contains_key("authorization"),
                "the POST probe must stay anonymous"
            );
            let body: serde_json::Value =
                serde_json::from_slice(&probe.body).expect("probe body is JSON");
            assert_eq!(body["method"], "initialize");
            assert_eq!(body["jsonrpc"], "2.0");
            assert!(
                body["params"]["protocolVersion"].is_string(),
                "the probe carries the transport protocol version"
            );
        }
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/custom/prm")
                .count(),
            1,
            "the hinted document is fetched once"
        );
    }

    #[tokio::test]
    async fn an_answered_post_probe_invents_no_authorization_layer() {
        // The follow-up POST must not manufacture OAuth: an endpoint that
        // answers the anonymous `initialize` without a challenge is not
        // asking for a bearer, so discovery still has nothing to follow and
        // fails actionably rather than inventing an authorization server.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&prm)
            .await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"jsonrpc": "2.0"})),
            )
            .mount(&prm)
            .await;

        let canonical = CanonicalResource::from_configured(
            &Url::parse(&format!("{}/mcp", prm.uri())).expect("fixture"),
        )
        .expect("canonical");
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical, None)
            .await
            .expect_err("an answer without a challenge is not an authorization layer");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/mcp" && r.method.as_str() == "POST")
                .count(),
            1,
            "one POST probe, no retry loop"
        );
        assert!(
            err.to_string().contains("protected-resource metadata"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn root_prm_serves_when_path_prm_missing() {
        let prm = mock_server().await;
        let asm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource"))
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

        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect("root PRM must serve");
        assert_eq!(
            discovery.protected_resource_url.as_str(),
            format!("{}/.well-known/oauth-protected-resource", prm.uri())
        );
        // The happy-ish path never pays for a probe.
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/mcp").count(),
            0
        );
    }

    #[tokio::test]
    async fn root_prm_declaring_origin_only_resource_is_rejected() {
        // A root document that declares only the ORIGIN as its resource
        // describes "this host does OAuth somewhere", not this MCP endpoint.
        // Keeping the strict canonical match is the impersonation rail; the
        // shape must be a hard failure with no probe rescue.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(prm_body(&prm.uri(), &["https://as.example".to_string()])),
            )
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("origin-only resource must fail");
        assert!(err.to_string().contains("does not match"), "got: {err}");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/mcp").count(),
            0
        );
    }

    #[tokio::test]
    async fn no_probe_on_happy_path() {
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
        let factory = OAuthClientFactory::new(&server_config());
        discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect("happy path");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/mcp").count(),
            0
        );
        assert_eq!(requests.len(), 1, "exactly one PRM fetch on happy path");
    }

    #[tokio::test]
    async fn present_but_invalid_prm_is_a_hard_failure() {
        // Path PRM answers 200 but declares the wrong resource: the chain
        // must not try the root form or probe; a lying document is not a
        // missing document.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                "https://elsewhere.example/mcp",
                &["https://as.example".to_string()],
            )))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("lying PRM must fail hard");
        assert!(
            err.to_string().contains("present but invalid"),
            "got: {err}"
        );
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests.iter().filter(|r| r.url.path() == "/mcp").count(),
            0
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/.well-known/oauth-protected-resource")
                .count(),
            0
        );
    }

    // ---- authorization-server candidate forms --------------------------

    #[tokio::test]
    async fn as_metadata_falls_back_to_oidc_inserted_form_for_path_issuer() {
        let prm = mock_server().await;
        let asm = mock_server().await;
        let issuer = format!("{}/tpp", asm.uri());
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                &format!("{}/mcp", prm.uri()),
                std::slice::from_ref(&issuer),
            )))
            .mount(&prm)
            .await;
        // Only the OIDC origin-inserted form serves.
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration/tpp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&issuer)))
            .mount(&asm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect("oidc-inserted form must be tried");
        assert_eq!(
            discovery.auth_server.token_endpoint.as_str(),
            format!("{issuer}/token")
        );
        let requests = asm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/.well-known/oauth-authorization-server/tpp")
                .count(),
            1,
            "the RFC 8414 form is tried first"
        );
    }

    #[tokio::test]
    async fn as_metadata_falls_back_to_oidc_appended_form_last_for_path_issuer() {
        let prm = mock_server().await;
        let asm = mock_server().await;
        let issuer = format!("{}/tpp", asm.uri());
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                &format!("{}/mcp", prm.uri()),
                std::slice::from_ref(&issuer),
            )))
            .mount(&prm)
            .await;
        // Only the OIDC path-APPENDED form serves: proves the third
        // candidate exists and runs after both inserted forms.
        Mock::given(method("GET"))
            .and(path("/tpp/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&issuer)))
            .mount(&asm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect("oidc-appended form must be tried last");
        assert_eq!(
            discovery.auth_server.token_endpoint.as_str(),
            format!("{issuer}/token")
        );
        let requests = asm.received_requests().await.expect("recorded");
        assert_eq!(requests.len(), 3, "all three candidates, in order");
    }

    #[tokio::test]
    async fn oidc_document_with_lying_issuer_is_rejected() {
        let prm = mock_server().await;
        let asm = mock_server().await;
        let issuer = format!("{}/tpp", asm.uri());
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                &format!("{}/mcp", prm.uri()),
                std::slice::from_ref(&issuer),
            )))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration/tpp"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(asm_body("http://127.0.0.1:9/elsewhere")),
            )
            .mount(&asm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("lying issuer must be rejected");
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }

    // ---- PRM-less resource-origin fallback -----------------------------

    #[tokio::test]
    async fn origin_openid_configuration_rescues_prm_less_server() {
        // The legacy same-origin shape: the MCP host IS the authorization
        // server, publishes no PRM at all, and challenges with a bare 401.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&prm.uri())))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let discovery = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect("origin fallback must rescue the legacy shape");
        assert!(
            super::super::metadata::issuer_matches(
                &discovery.auth_server.issuer,
                &Url::parse(&prm.uri()).expect("fixture")
            ),
            "the origin must be the issuer, got {}",
            discovery.auth_server.issuer
        );
        assert_eq!(
            discovery.protected_resource_url.as_str(),
            format!("{}/.well-known/openid-configuration", prm.uri()),
            "the fallback records the document that backed the result"
        );
    }

    #[tokio::test]
    async fn origin_as_fallback_rejects_issuer_not_equal_to_origin() {
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(asm_body("https://elsewhere.example")),
            )
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("off-site origin issuer must be refused");
        let text = err.to_string();
        assert!(text.contains("does not match"), "got: {text}");
        assert!(
            text.contains("probe"),
            "probe outcome missing from trail: {text}"
        );
    }

    #[tokio::test]
    async fn origin_as_fallback_requires_probe_attestation() {
        // The probe answered 200: the server is not asking for OAuth, so
        // origin-pinned rescue candidates must not be fetched at all.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("no metadata anywhere must fail");
        assert!(
            err.to_string().contains("without a bearer challenge"),
            "got: {err}"
        );
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path().starts_with("/.well-known"))
                .count(),
            2,
            "path and root PRM forms only; no origin AS fetch"
        );
    }

    #[tokio::test]
    async fn an_error_answer_that_is_not_a_challenge_does_not_unlock_the_origin_as() {
        // A 404 off a misconfigured path attests that the host answered, not
        // that it is protected, so the origin-pinned authorization-server
        // candidates must not be fetched on that evidence alone.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&prm.uri())))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("an error status without a challenge must not invent an AS");
        let text = err.to_string();
        assert!(text.contains("HTTP 404"), "got: {text}");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/.well-known/oauth-authorization-server")
                .count(),
            0,
            "the origin AS candidate must not be fetched without a bearer challenge"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path().starts_with("/.well-known"))
                .count(),
            2,
            "the two protected-resource forms only"
        );
    }

    #[tokio::test]
    async fn probe_hint_violating_policy_is_named_never_fetched() {
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                r#"Bearer resource_metadata="http://169.254.169.254/prm""#,
            ))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("nothing valid here");
        let text = err.to_string();
        assert!(
            text.contains("rejected by the URL policy"),
            "hostile hint must be named: {text}"
        );
        // The link-literal host was never fetched (no hang, no request).
        assert!(
            !text.contains("169.254.169.254/prm →"),
            "hint must not be fetched: {text}"
        );
    }

    #[tokio::test]
    async fn issuers_beyond_the_cap_are_named_not_probed() {
        let prm = mock_server().await;
        let mut issuers: Vec<String> = (1..=4)
            .map(|i| format!("http://127.0.0.1:1/i{i}"))
            .collect();
        // Issuers 5 and 6 sit on a live mock so the cap is pinned by
        // request count, not only by the error naming them.
        let over_cap = mock_server().await;
        issuers.push(format!("{}/i5", over_cap.uri()));
        issuers.push(format!("{}/i6", over_cap.uri()));
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(prm_body(&format!("{}/mcp", prm.uri()), &issuers)),
            )
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("connection-refused issuers must fail");
        let text = err.to_string();
        assert!(text.contains("not tried (issuer cap 4)"), "got: {text}");
        assert!(
            text.contains("/i5"),
            "skipped issuers must be named: {text}"
        );
        assert!(
            over_cap
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "over-cap issuers must not be probed"
        );
    }

    #[tokio::test]
    async fn probe_hint_equal_to_tried_url_is_not_refetched() {
        // The common server shape: the 401 advertises the SAME default path
        // form that just 404'd. The hint must be named as already tried,
        // not fetched a second time.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource/mcp""#,
                    prm.uri()
                ),
            ))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("nothing valid here");
        assert!(err.to_string().contains("not fetched again"), "got: {err}");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/.well-known/oauth-protected-resource/mcp")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn probe_hint_equal_to_root_form_is_not_refetched() {
        // The dedup covers EVERY URL the pass already tried, not just the
        // default path form: a hint pointing at the root well-known URL
        // that just 404'd must not be fetched a second time.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer resource_metadata="{}/.well-known/oauth-protected-resource""#,
                    prm.uri()
                ),
            ))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("nothing valid here");
        assert!(err.to_string().contains("not fetched again"), "got: {err}");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/.well-known/oauth-protected-resource")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn probe_hint_equal_to_challenge_hint_is_not_refetched() {
        // The RFC 9728 §5.2 steady shape: every 401 (including the one
        // that triggered the re-discovery) advertises the SAME custom URL.
        // When that document is gone, the pass must fetch it exactly once.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(r#"Bearer resource_metadata="{}/gone/prm""#, prm.uri()),
            ))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let challenge = format!(
            r#"Bearer error="invalid_token", resource_metadata="{}/gone/prm""#,
            prm.uri()
        );
        let err = discover_oauth(&factory, &canonical_for(&prm), Some(&challenge))
            .await
            .expect_err("advertised document is gone");
        assert!(err.to_string().contains("not fetched again"), "got: {err}");
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/gone/prm")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn probe_advertised_broken_prm_is_a_hard_failure() {
        // Same rule everywhere: a document that EXISTS and LIES is a hard
        // failure, even when the probe advertised the location. The
        // origin-AS fallback must not rescue over a contract-violating
        // document the server itself pointed at.
        let prm = mock_server().await;
        Mock::given(method("GET"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(r#"Bearer resource_metadata="{}/custom/prm""#, prm.uri()),
            ))
            .mount(&prm)
            .await;
        Mock::given(method("GET"))
            .and(path("/custom/prm"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm_body(
                "https://elsewhere.example/mcp",
                &["https://as.example".to_string()],
            )))
            .mount(&prm)
            .await;
        // A VALID origin AS document is mounted: only a soft Miss would
        // let the fallback reach it.
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(asm_body(&prm.uri())))
            .mount(&prm)
            .await;
        let factory = OAuthClientFactory::new(&server_config());
        let err = discover_oauth(&factory, &canonical_for(&prm), None)
            .await
            .expect_err("lying advertised document must fail hard");
        assert!(
            err.to_string().contains("present but invalid"),
            "got: {err}"
        );
        let requests = prm.received_requests().await.expect("recorded");
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.path() == "/.well-known/openid-configuration")
                .count(),
            0,
            "the origin fallback must not rescue a hard failure"
        );
    }
}
