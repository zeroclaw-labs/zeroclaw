//! RFC 7591 dynamic client registration for public clients.
//!
//! ZeroClaw registers itself per MCP authorization server as a public client
//! (`token_endpoint_auth_method = none`): no client secret is ever kept, so
//! there is no secret to store, rotate, or leak. A configured
//! `oauth_client_id` skips registration entirely.

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Deserialize;

use super::policy::MAX_METADATA_BYTES;
use super::truncate_untrusted;

/// A registered public client. Deliberately carries no secret: a secret the
/// AS insists on returning is logged at DEBUG (level, name only) and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredClient {
    pub client_id: String,
}

#[derive(Deserialize)]
struct RegistrationResponse {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    /// RFC 7591 §3.2.1 success members, when the AS echoes what it issued;
    /// checked fail-closed below.
    #[serde(default)]
    redirect_uris: Option<Vec<String>>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    /// RFC 7591 §3.2.2 error members (only present on error responses).
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Upper bound on a returned client id: RFC 7591 leaves it AS-chosen, but
/// the id rides the authorization URL and every request afterwards, so a
/// megabyte-long "id" is a hostile body, not an identifier.
const MAX_CLIENT_ID_BYTES: usize = 1024;

/// The client_id alphabet accepted at config validation (see
/// `validate_mcp_oauth_fields`): the AS's returned id must pass the same
/// gate, because it goes verbatim into the authorization URL.
fn client_id_is_usable(client_id: &str) -> bool {
    !client_id.is_empty()
        && client_id.len() <= MAX_CLIENT_ID_BYTES
        && client_id
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x5B | 0x5D..=0x7E))
}

/// POST a RFC 7591 registration document to `endpoint` (from validated AS
/// metadata) and return the issued public client. `client` MUST come from
/// `OAuthClientFactory::client_for(endpoint)`: the endpoint is AS-supplied,
/// and the per-target client is what carries the URL policy + DNS pinning.
/// `redirect_uri` is the
/// loopback callback this exact flow bound, so it must be registered before
/// the authorization request is built.
pub async fn register_client(
    client: &reqwest::Client,
    endpoint: &Url,
    redirect_uri: &str,
    scopes: Option<&[String]>,
) -> Result<RegisteredClient> {
    let mut document = serde_json::json!({
        "client_name": "ZeroClaw MCP client",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(scopes) = scopes
        && !scopes.is_empty()
    {
        // Same single-joined-scope convention as the authorization request
        // (RFC 6749 §3.3 space-delimited list).
        document["scope"] = serde_json::Value::String(scopes.join(" "));
    }

    let response = client
        .post(endpoint.as_str())
        // RFC 7591 §3.3 speaks JSON; ask for it explicitly (S2-3, same
        // content-negotiation posture as the token endpoint).
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&document)
        .send()
        .await
        .with_context(|| format!("failed to reach the registration endpoint {endpoint}"))?;
    let status = response.status();
    let body =
        crate::mcp_transport::read_body_bounded_to_string(response, MAX_METADATA_BYTES).await?;
    let parsed: RegistrationResponse = if status.is_success() {
        serde_json::from_str(&body).with_context(|| {
            format!(
                "registration endpoint {endpoint} returned HTTP {status} with a non-JSON body ({} bytes)",
                body.len()
            )
        })?
    } else {
        // Error bodies are JSON per RFC 7591 §3.2.2, but servers are loose;
        // a non-JSON error still needs an actionable failure, not a parse
        // report.
        serde_json::from_str(&body).unwrap_or(RegistrationResponse {
            client_id: None,
            client_secret: None,
            redirect_uris: None,
            token_endpoint_auth_method: None,
            error: None,
            error_description: Some(truncate_untrusted(&body)),
        })
    };

    if !status.is_success() {
        let error =
            truncate_untrusted(&parsed.error.unwrap_or_else(|| "unknown_error".to_string()));
        let detail = parsed
            .error_description
            .map(|d| format!(": {}", truncate_untrusted(&d)))
            .unwrap_or_default();
        bail!("dynamic registration at {endpoint} failed: HTTP {status} {error}{detail}");
    }

    let client_id = parsed
        .client_id
        .context("dynamic registration succeeded but returned no client_id")?;
    if !client_id_is_usable(&client_id) {
        bail!(
            "dynamic registration returned a client_id outside the accepted client-id shape (alphabet or {MAX_CLIENT_ID_BYTES}-byte cap); refusing to build an authorization URL around it"
        );
    }
    // Fail-closed echo verification (RFC 7591 §3.2.1): when the AS states
    // what it issued, it must match what this flow requires — this exact
    // loopback redirect and public-client authentication.
    if let Some(redirect_uris) = parsed.redirect_uris.as_ref()
        && !redirect_uris.iter().any(|uri| uri == redirect_uri)
    {
        bail!(
            "dynamic registration issued redirect_uris that do not include this flow's callback URI; refusing to continue"
        );
    }
    if let Some(method) = parsed.token_endpoint_auth_method.as_deref()
        && method != "none"
    {
        bail!(
            "dynamic registration issued token_endpoint_auth_method {:?}; this client authenticates as public (none) only",
            truncate_untrusted(method)
        );
    }
    if parsed.client_secret.is_some() {
        // Public client: keep nothing. The value intentionally never
        // reaches logs or errors.
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"mcp_oauth": "dynamic-registration"})),
            "MCP OAuth dynamic registration returned a client_secret; discarded (public client)"
        );
    }
    Ok(RegisteredClient { client_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_oauth::policy::OAuthClientFactory;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn server_config() -> zeroclaw_config::schema::McpServerConfig {
        zeroclaw_config::schema::McpServerConfig {
            name: "registration-test".to_string(),
            // Mirrors production: the configured URL is the trust anchor for
            // loopback/private endpoints (fetches here hit loopback mocks).
            url: Some("http://127.0.0.1:1/register".to_string()),
            ..Default::default()
        }
    }

    async fn client_for(server: &MockServer, path_str: &str) -> reqwest::Client {
        let url = Url::parse(&format!("{}{path_str}", server.uri())).expect("url");
        OAuthClientFactory::new(&server_config())
            .client_for(&url)
            .await
            .expect("client")
    }

    #[tokio::test]
    async fn rejects_echoed_redirect_uris_that_omit_our_callback() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "client_id": "cid",
                "redirect_uris": ["https://attacker.example/cb"],
            })))
            .mount(&server)
            .await;
        let client = client_for(&server, "/register").await;
        let endpoint = Url::parse(&format!("{}/register", server.uri())).expect("url");
        let err = register_client(&client, &endpoint, "http://127.0.0.1:9/callback", None)
            .await
            .expect_err("echoed redirect_uris must be verified");
        assert!(err.to_string().contains("redirect_uris"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_secret_auth_echo_and_oversized_client_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "client_id": "cid",
                "token_endpoint_auth_method": "client_secret_basic",
            })))
            .mount(&server)
            .await;
        let client = client_for(&server, "/register").await;
        let endpoint = Url::parse(&format!("{}/register", server.uri())).expect("url");
        let err = register_client(&client, &endpoint, "http://127.0.0.1:9/callback", None)
            .await
            .expect_err("a secret-auth echo cannot be satisfied by a public client");
        assert!(
            err.to_string().contains("token_endpoint_auth_method"),
            "got: {err}"
        );

        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "client_id": "x".repeat(1025),
            })))
            .mount(&server2)
            .await;
        let client2 = client_for(&server2, "/register").await;
        let endpoint2 = Url::parse(&format!("{}/register", server2.uri())).expect("url");
        let err = register_client(&client2, &endpoint2, "http://127.0.0.1:9/callback", None)
            .await
            .expect_err("an oversized AS-chosen id is a hostile body, not an identifier");
        assert!(err.to_string().contains("shape"), "got: {err}");
    }

    #[tokio::test]
    async fn registers_public_client_and_discards_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .and(header("content-type", "application/json"))
            // S2-3 (NIT-1, reviewer A): DCR must also ASK for JSON, not
            // only send it.
            .and(header("accept", "application/json"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "client_id": "issued-id-123",
                "client_secret": "must-not-be-kept",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = client_for(&server, "/register").await;
        let registered = register_client(
            &client,
            &Url::parse(&format!("{}/register", server.uri())).expect("url"),
            "http://127.0.0.1:9/callback",
            Some(&["openid".to_string(), "mcp".to_string()]),
        )
        .await
        .expect("registered");
        assert_eq!(registered.client_id, "issued-id-123");
        // The request must be an exact public-client registration document.
        let requests = server.received_requests().await.expect("received");
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("json body");
        assert_eq!(body["token_endpoint_auth_method"], "none");
        assert_eq!(
            body["grant_types"],
            serde_json::json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(body["response_types"], serde_json::json!(["code"]));
        assert_eq!(
            body["redirect_uris"],
            serde_json::json!(["http://127.0.0.1:9/callback"])
        );
        assert_eq!(body["scope"], "openid mcp");
    }

    #[tokio::test]
    async fn registration_error_response_is_actionable_not_a_parse_report() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_redirect_uri",
                "error_description": "loopback not allowed",
            })))
            .mount(&server)
            .await;
        let client = client_for(&server, "/register").await;
        let err = register_client(
            &client,
            &Url::parse(&format!("{}/register", server.uri())).expect("url"),
            "http://127.0.0.1:9/callback",
            None,
        )
        .await
        .expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("invalid_redirect_uri"), "got: {message}");
        assert!(message.contains("loopback not allowed"), "got: {message}");
    }

    #[tokio::test]
    async fn non_json_error_body_still_fails_actionably() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_string("<html>gateway blew up</html>")
                    .insert_header("content-type", "text/html"),
            )
            .mount(&server)
            .await;
        let client = client_for(&server, "/register").await;
        let err = register_client(
            &client,
            &Url::parse(&format!("{}/register", server.uri())).expect("url"),
            "http://127.0.0.1:9/callback",
            None,
        )
        .await
        .expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("HTTP 500"), "got: {message}");
        assert!(message.contains("gateway blew up"), "got: {message}");
    }

    #[tokio::test]
    async fn missing_or_unsafe_client_id_is_rejected() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({"client_id": ""}),
            serde_json::json!({"client_id": "has space"}),
            serde_json::json!({"client_id": "quote\"d"}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/register"))
                .respond_with(ResponseTemplate::new(201).set_body_json(body.clone()))
                .mount(&server)
                .await;
            let client = client_for(&server, "/register").await;
            register_client(
                &client,
                &Url::parse(&format!("{}/register", server.uri())).expect("url"),
                "http://127.0.0.1:9/callback",
                None,
            )
            .await
            .expect_err("body must fail closed");
        }
    }

    #[tokio::test]
    async fn oversized_registration_body_is_bounded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_string(vec![b' '; MAX_METADATA_BYTES as usize + 1])
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;
        let client = client_for(&server, "/register").await;
        let err = register_client(
            &client,
            &Url::parse(&format!("{}/register", server.uri())).expect("url"),
            "http://127.0.0.1:9/callback",
            None,
        )
        .await
        .expect_err("oversized body must fail");
        assert!(err.to_string().contains("exceeds"), "got: {err}");
    }
}
