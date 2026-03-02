//! MCP transport abstraction — supports stdio, SSE, and HTTP transports.

use std::borrow::Cow;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex, Notify};
use tokio::time::{timeout, Duration};
use tokio_stream::StreamExt;

use crate::config::schema::{McpServerConfig, McpTransport};
use crate::tools::mcp_protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, INTERNAL_ERROR};

/// Maximum bytes for a single JSON-RPC response.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024; // 4 MB

/// Timeout for init/list operations.
const RECV_TIMEOUT_SECS: u64 = 30;

/// Streamable HTTP Accept header required by MCP HTTP transport.
const MCP_STREAMABLE_ACCEPT: &str = "application/json, text/event-stream";

/// Default media type for MCP JSON-RPC request bodies.
const MCP_JSON_CONTENT_TYPE: &str = "application/json";

// ── Transport Trait ──────────────────────────────────────────────────────

/// Abstract transport for MCP communication.
#[async_trait::async_trait]
pub trait McpTransportConn: Send + Sync {
    /// Send a JSON-RPC request and receive the response.
    async fn send_and_recv(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse>;

    /// Close the connection.
    async fn close(&mut self) -> Result<()>;
}

// ── Stdio Transport ──────────────────────────────────────────────────────

/// Stdio-based transport (spawn local process).
pub struct StdioTransport {
    _child: Child,
    stdin: tokio::process::ChildStdin,
    stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl StdioTransport {
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn MCP server `{}`", config.name))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("no stdin on MCP server `{}`", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout on MCP server `{}`", config.name))?;
        let stdout_lines = BufReader::new(stdout).lines();

        Ok(Self {
            _child: child,
            stdin,
            stdout_lines,
        })
    }

    async fn send_raw(&mut self, line: &str) -> Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .await
            .context("failed to write to MCP server stdin")?;
        self.stdin
            .write_all(b"\n")
            .await
            .context("failed to write newline to MCP server stdin")?;
        self.stdin.flush().await.context("failed to flush stdin")?;
        Ok(())
    }

    async fn recv_raw(&mut self) -> Result<String> {
        let line = self
            .stdout_lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("MCP server closed stdout"))?;
        if line.len() > MAX_LINE_BYTES {
            bail!("MCP response too large: {} bytes", line.len());
        }
        Ok(line)
    }
}

#[async_trait::async_trait]
impl McpTransportConn for StdioTransport {
    async fn send_and_recv(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        let line = serde_json::to_string(request)?;
        self.send_raw(&line).await?;
        if request.id.is_none() {
            return Ok(JsonRpcResponse {
                jsonrpc: crate::tools::mcp_protocol::JSONRPC_VERSION.to_string(),
                id: None,
                result: None,
                error: None,
            });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(RECV_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                bail!("timeout waiting for MCP response");
            }
            let resp_line = timeout(remaining, self.recv_raw())
                .await
                .context("timeout waiting for MCP response")??;
            let resp: JsonRpcResponse = serde_json::from_str(&resp_line)
                .with_context(|| format!("invalid JSON-RPC response: {}", resp_line))?;
            if resp.id.is_none() {
                // Server-sent notification (e.g. `notifications/initialized`) — skip and
                // keep waiting for the actual response to our request.
                tracing::debug!(
                    "MCP stdio: skipping server notification while waiting for response"
                );
                continue;
            }
            return Ok(resp);
        }
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.stdin.shutdown().await;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OAuthTokenCache {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
    server_url: String,
}

impl OAuthTokenCache {
    fn cache_dir() -> std::path::PathBuf {
        directories::UserDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".zeroclaw")
            .join("mcp-oauth-tokens")
    }

    fn cache_path(server_name: &str) -> std::path::PathBuf {
        let safe_name: String = server_name
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        Self::cache_dir().join(format!("{safe_name}.json"))
    }

    fn load(server_name: &str, server_url: &str) -> Option<Self> {
        let path = Self::cache_path(server_name);
        let contents = std::fs::read_to_string(path).ok()?;
        let cache: Self = serde_json::from_str(&contents).ok()?;
        if cache.server_url != server_url {
            return None;
        }
        Some(cache)
    }

    fn save(&self, server_name: &str) {
        let cache_dir = Self::cache_dir();
        if let Err(err) = std::fs::create_dir_all(&cache_dir) {
            tracing::warn!("Failed to create MCP OAuth cache dir: {err}");
            return;
        }

        let path = Self::cache_path(server_name);
        let serialized = match serde_json::to_string_pretty(self) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("Failed to serialize MCP OAuth token cache: {err}");
                return;
            }
        };

        if let Err(err) = std::fs::write(path, serialized) {
            tracing::warn!("Failed to write MCP OAuth token cache: {err}");
        }
    }

    fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(expires_at) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                now >= expires_at.saturating_sub(60)
            }
            None => false,
        }
    }
}

// ── HTTP Transport ───────────────────────────────────────────────────────

/// HTTP-based transport (POST requests).
pub struct HttpTransport {
    url: String,
    server_name: String,
    client: reqwest::Client,
    headers: std::collections::HashMap<String, String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

impl HttpTransport {
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let url = config
            .url
            .as_ref()
            .ok_or_else(|| anyhow!("URL required for HTTP transport"))?
            .clone();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build HTTP client")?;

        let (access_token, refresh_token) = match OAuthTokenCache::load(&config.name, &url) {
            Some(cache) if !cache.is_expired() => (Some(cache.access_token), cache.refresh_token),
            Some(cache) if cache.refresh_token.is_some() => (None, cache.refresh_token),
            _ => (None, None),
        };

        Ok(Self {
            url,
            server_name: config.name.clone(),
            client,
            headers: config.headers.clone(),
            access_token,
            refresh_token,
        })
    }
}

#[async_trait::async_trait]
impl McpTransportConn for HttpTransport {
    async fn send_and_recv(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        let body = serde_json::to_string(request)?;

        let resp = self.http_post(&body).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                if let Some(refresh_token) = self.refresh_token.clone() {
                    if let Ok(token_cache) =
                        try_refresh_token(&self.client, &self.url, &refresh_token, &resp).await
                    {
                        self.access_token = Some(token_cache.access_token.clone());
                        self.refresh_token = token_cache.refresh_token.clone();
                        token_cache.save(&self.server_name);

                        let retry_resp = self.http_post(&body).await?;
                        if retry_resp.status().is_success() {
                            return self.parse_response(retry_resp, request).await;
                        }
                    }
                }

                match perform_mcp_oauth_flow(&self.client, &self.url, &resp).await {
                    Ok(token_cache) => {
                        self.access_token = Some(token_cache.access_token.clone());
                        self.refresh_token = token_cache.refresh_token.clone();
                        token_cache.save(&self.server_name);
                        let retry_resp = self.http_post(&body).await?;
                        if !retry_resp.status().is_success() {
                            bail!(
                                "MCP server returned HTTP {} after OAuth",
                                retry_resp.status()
                            );
                        }
                        return self.parse_response(retry_resp, request).await;
                    }
                    Err(e) => {
                        bail!("MCP server returned HTTP {status}: OAuth flow failed: {e:#}");
                    }
                }
            }
            bail!("MCP server returned HTTP {status}");
        }

        self.parse_response(resp, request).await
    }

    async fn close(&mut self) -> Result<()> {
        Ok(())
    }
}

impl HttpTransport {
    /// Build and send an HTTP POST to the MCP server, injecting auth + standard headers.
    async fn http_post(&self, body: &str) -> Result<reqwest::Response> {
        let has_accept = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Accept"));
        let has_content_type = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Content-Type"));

        let mut req = self.client.post(&self.url).body(body.to_string());
        if !has_content_type {
            req = req.header("Content-Type", MCP_JSON_CONTENT_TYPE);
        }
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        if !has_accept {
            req = req.header("Accept", MCP_STREAMABLE_ACCEPT);
        }
        if let Some(token) = &self.access_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req.send()
            .await
            .context("HTTP request to MCP server failed")
    }

    /// Parse a successful MCP response (JSON or SSE-framed).
    async fn parse_response(
        &self,
        resp: reqwest::Response,
        request: &JsonRpcRequest,
    ) -> Result<JsonRpcResponse> {
        if request.id.is_none() {
            return Ok(JsonRpcResponse {
                jsonrpc: crate::tools::mcp_protocol::JSONRPC_VERSION.to_string(),
                id: None,
                result: None,
                error: None,
            });
        }

        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
        if is_sse {
            let maybe_resp = timeout(
                Duration::from_secs(RECV_TIMEOUT_SECS),
                read_first_jsonrpc_from_sse_response(resp),
            )
            .await
            .context("timeout waiting for MCP response from streamable HTTP SSE stream")??;
            return maybe_resp
                .ok_or_else(|| anyhow!("MCP server returned no response in SSE stream"));
        }

        let resp_text = resp.text().await.context("failed to read HTTP response")?;
        parse_jsonrpc_response_text(&resp_text)
    }
}

// ── MCP OAuth (RFC 9728 → RFC 8414 → OAuth 2.1 PKCE) ────────────────────

/// Callback port for the MCP OAuth loopback server.
const MCP_OAUTH_CALLBACK_PORT: u16 = 1457;

/// OAuth metadata discovered from the MCP server's authorization server.
struct OAuthMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

async fn try_refresh_token(
    client: &reqwest::Client,
    server_url: &str,
    refresh_token: &str,
    resp: &reqwest::Response,
) -> Result<OAuthTokenCache> {
    let metadata = discover_oauth_metadata(client, server_url, resp).await?;
    let creds = resolve_client_id(client, &metadata).await?;

    if !metadata.token_endpoint.starts_with("https://") {
        bail!("Refusing to send credentials to non-HTTPS token endpoint");
    }

    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("resource", server_url.to_string()),
    ];
    let mut req = client.post(&metadata.token_endpoint);

    match creds.auth_method.as_str() {
        "client_secret_basic" => {
            let secret = creds
                .client_secret
                .as_deref()
                .ok_or_else(|| anyhow!("client_secret_basic requires client_secret"))?;
            req = req.basic_auth(&creds.client_id, Some(secret));
        }
        "client_secret_post" => {
            let secret = creds
                .client_secret
                .as_ref()
                .ok_or_else(|| anyhow!("client_secret_post requires client_secret"))?;
            form.push(("client_id", creds.client_id.clone()));
            form.push(("client_secret", secret.clone()));
        }
        "none" => {
            form.push(("client_id", creds.client_id.clone()));
        }
        other => {
            bail!("Unsupported token_endpoint_auth_method: {other}");
        }
    }

    let token_resp = req
        .form(&form)
        .send()
        .await
        .context("Refresh token exchange request failed")?;
    let status = token_resp.status();
    let body = token_resp
        .text()
        .await
        .context("Failed to read refresh token response")?;

    if !status.is_success() {
        bail!("Refresh token exchange failed (HTTP {status})");
    }

    let token_json: serde_json::Value =
        serde_json::from_str(&body).context("Invalid refresh token response JSON")?;
    let access_token = token_json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Refresh response missing access_token"))?
        .to_string();
    let refreshed_token = token_json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| Some(refresh_token.to_string()));
    let expires_at = token_json
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .map(|secs| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + secs
        });

    Ok(OAuthTokenCache {
        access_token,
        refresh_token: refreshed_token,
        expires_at,
        server_url: server_url.to_string(),
    })
}

/// Run the full MCP OAuth authorization code flow with PKCE (RFC 9728 → RFC 8414).
async fn perform_mcp_oauth_flow(
    client: &reqwest::Client,
    server_url: &str,
    resp: &reqwest::Response,
) -> Result<OAuthTokenCache> {
    use crate::auth::oauth_common::{generate_pkce_state, url_encode};
    use tokio::net::TcpListener;

    let metadata = discover_oauth_metadata(client, server_url, resp)
        .await
        .context("Failed to discover MCP OAuth endpoints")?;

    tracing::info!(
        authorization_endpoint = %metadata.authorization_endpoint,
        token_endpoint = %metadata.token_endpoint,
        "MCP OAuth metadata discovered"
    );

    let creds = resolve_client_id(client, &metadata).await?;

    let pkce = generate_pkce_state();
    let redirect_uri = format!("http://127.0.0.1:{MCP_OAUTH_CALLBACK_PORT}/callback");
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&resource={}",
        metadata.authorization_endpoint,
        url_encode(&creds.client_id),
        url_encode(&redirect_uri),
        url_encode(&pkce.state),
        url_encode(&pkce.code_challenge),
        url_encode(server_url),
    );

    // Bind listener before opening the browser to avoid racing the redirect.
    let listener = TcpListener::bind(format!("127.0.0.1:{MCP_OAUTH_CALLBACK_PORT}"))
        .await
        .context("Failed to bind MCP OAuth callback listener")?;

    eprintln!("\n🔑 MCP OAuth: Opening browser for authorization...");
    eprintln!("   If the browser doesn't open, check the terminal for the authorization URL.\n");
    tracing::debug!("MCP OAuth authorization URL prepared");

    let _ = open_browser_url(&auth_url);

    eprintln!("   Waiting for callback at {redirect_uri} ...");

    let (code, received_state) =
        tokio::time::timeout(Duration::from_secs(120), receive_oauth_callback(&listener))
            .await
            .context("OAuth callback timed out (120s)")?
            .context("Failed to receive OAuth callback")?;

    if received_state != pkce.state {
        bail!("OAuth state mismatch — possible CSRF attack");
    }

    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", pkce.code_verifier),
        ("resource", server_url.to_string()),
    ];

    if !metadata.token_endpoint.starts_with("https://") {
        bail!("Refusing to send credentials to non-HTTPS token endpoint");
    }

    let mut req = client.post(&metadata.token_endpoint);

    match creds.auth_method.as_str() {
        "client_secret_basic" => {
            let secret = creds
                .client_secret
                .as_deref()
                .ok_or_else(|| anyhow!("client_secret_basic requires client_secret"))?;
            req = req.basic_auth(&creds.client_id, Some(secret));
        }
        "client_secret_post" => {
            let secret = creds
                .client_secret
                .as_ref()
                .ok_or_else(|| anyhow!("client_secret_post requires client_secret"))?;
            form.push(("client_id", creds.client_id.clone()));
            form.push(("client_secret", secret.clone()));
        }
        "none" => {
            form.push(("client_id", creds.client_id.clone()));
        }
        other => {
            bail!("Unsupported token_endpoint_auth_method: {other}");
        }
    }

    let token_resp = req
        .form(&form)
        .send()
        .await
        .context("Token exchange request failed")?;

    let status = token_resp.status();
    let body = token_resp
        .text()
        .await
        .context("Failed to read token response")?;

    if !status.is_success() {
        bail!("Token exchange failed (HTTP {status})");
    }

    let token_json: serde_json::Value =
        serde_json::from_str(&body).context("Invalid token response JSON")?;
    let access_token = token_json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Token response missing access_token"))?
        .to_string();
    let refresh_token = token_json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let expires_at = token_json
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .map(|secs| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + secs
        });

    eprintln!("   ✅ MCP OAuth: Authenticated successfully\n");
    Ok(OAuthTokenCache {
        access_token,
        refresh_token,
        expires_at,
        server_url: server_url.to_string(),
    })
}

/// Discover OAuth metadata for an MCP server (RFC 9728 → RFC 8414).
async fn discover_oauth_metadata(
    client: &reqwest::Client,
    server_url: &str,
    resp: &reqwest::Response,
) -> Result<OAuthMetadata> {
    // Extract resource_metadata URL from WWW-Authenticate header.
    let resource_metadata_url = resp
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .and_then(|header| {
            for part in header.split(',') {
                let part = part.trim();
                if let Some(idx) = part.find("resource_metadata=\"") {
                    let start = idx + "resource_metadata=\"".len();
                    let rest = &part[start..];
                    return rest.strip_suffix('"').map(str::to_string);
                }
            }
            None
        });

    let parsed = reqwest::Url::parse(server_url).context("Invalid MCP server URL")?;
    let origin = format!("{}://{}", parsed.scheme(), parsed.authority());
    let path = parsed.path().trim_end_matches('/');

    let candidates: Vec<String> = if let Some(url) = resource_metadata_url {
        let header_url = reqwest::Url::parse(&url).context("Invalid resource_metadata URL")?;
        if header_url.scheme() != "https" {
            bail!("Refusing non-HTTPS resource_metadata URL");
        }
        if header_url.host_str() != parsed.host_str()
            || header_url.port_or_known_default() != parsed.port_or_known_default()
        {
            bail!("Refusing cross-origin resource_metadata URL");
        }
        vec![header_url.to_string()]
    } else {
        let mut urls = Vec::new();
        if !path.is_empty() && path != "/" {
            urls.push(format!(
                "{origin}/.well-known/oauth-protected-resource{path}"
            ));
        }
        urls.push(format!("{origin}/.well-known/oauth-protected-resource"));
        urls
    };

    // Fetch Protected Resource Metadata → extract authorization_servers[0].
    let mut auth_server: Option<String> = None;
    for url in &candidates {
        if let Ok(r) = client.get(url).send().await {
            if r.status().is_success() {
                if let Ok(json) = r.json::<serde_json::Value>().await {
                    if let Some(servers) =
                        json.get("authorization_servers").and_then(|v| v.as_array())
                    {
                        auth_server = servers.first().and_then(|v| v.as_str()).map(String::from);
                    }
                    if auth_server.is_some() {
                        break;
                    }
                }
            }
        }
    }

    let auth_server =
        auth_server.context("Could not discover authorization server from resource metadata")?;

    if !auth_server.starts_with("https://") {
        bail!("Refusing non-HTTPS authorization server URL: {auth_server}");
    }

    // Fetch Authorization Server Metadata (RFC 8414 / OIDC Discovery).
    let as_parsed = reqwest::Url::parse(&auth_server)?;
    let as_origin = format!("{}://{}", as_parsed.scheme(), as_parsed.authority());
    let as_path = as_parsed.path().trim_end_matches('/');

    let mut as_metadata_urls = Vec::new();
    if !as_path.is_empty() && as_path != "/" {
        as_metadata_urls.push(format!(
            "{as_origin}/.well-known/oauth-authorization-server{as_path}"
        ));
        as_metadata_urls.push(format!(
            "{as_origin}/.well-known/openid-configuration{as_path}"
        ));
    }
    as_metadata_urls.push(format!(
        "{as_origin}/.well-known/oauth-authorization-server"
    ));
    as_metadata_urls.push(format!("{as_origin}/.well-known/openid-configuration"));

    for url in &as_metadata_urls {
        if let Ok(r) = client.get(url).send().await {
            if r.status().is_success() {
                if let Ok(json) = r.json::<serde_json::Value>().await {
                    if let (Some(authz), Some(token)) = (
                        json.get("authorization_endpoint").and_then(|v| v.as_str()),
                        json.get("token_endpoint").and_then(|v| v.as_str()),
                    ) {
                        let authz_url = reqwest::Url::parse(authz)
                            .context("Invalid authorization_endpoint URL")?;
                        let token_url =
                            reqwest::Url::parse(token).context("Invalid token_endpoint URL")?;
                        if authz_url.scheme() != "https" || token_url.scheme() != "https" {
                            bail!("Refusing non-HTTPS OAuth endpoints");
                        }
                        return Ok(OAuthMetadata {
                            authorization_endpoint: authz_url.to_string(),
                            token_endpoint: token_url.to_string(),
                            registration_endpoint: json
                                .get("registration_endpoint")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                        });
                    }
                }
            }
        }
    }

    bail!("Could not discover OAuth authorization/token endpoints from {auth_server}")
}

/// Resolved OAuth client credentials.
struct OAuthClientCreds {
    client_id: String,
    client_secret: Option<String>,
    /// One of: "none", "client_secret_basic", "client_secret_post"
    auth_method: String,
}

/// Determine client_id — use dynamic registration if available, else use server URL.
async fn resolve_client_id(
    client: &reqwest::Client,
    metadata: &OAuthMetadata,
) -> Result<OAuthClientCreds> {
    if let Some(reg_endpoint) = &metadata.registration_endpoint {
        let redirect_uri = format!("http://127.0.0.1:{MCP_OAUTH_CALLBACK_PORT}/callback");
        let reg_body = serde_json::json!({
            "client_name": "ZeroClaw MCP Client",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "client_secret_basic"
        });

        if let Ok(resp) = client.post(reg_endpoint).json(&reg_body).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(id) = json.get("client_id").and_then(|v| v.as_str()) {
                        let secret = json
                            .get("client_secret")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let auth_method = json
                            .get("token_endpoint_auth_method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("client_secret_basic")
                            .to_string();
                        tracing::info!(
                            client_id = id,
                            auth_method = %auth_method,
                            "MCP OAuth: dynamically registered"
                        );
                        return Ok(OAuthClientCreds {
                            client_id: id.to_string(),
                            client_secret: secret,
                            auth_method,
                        });
                    }
                }
            }
        }
        tracing::warn!("MCP OAuth: dynamic registration failed, falling back to default client_id");
    }

    Ok(OAuthClientCreds {
        client_id: "zeroclaw".to_string(),
        client_secret: None,
        auth_method: "none".to_string(),
    })
}

/// Accept a single OAuth callback on the loopback listener, return (code, state).
async fn receive_oauth_callback(listener: &tokio::net::TcpListener) -> Result<(String, String)> {
    use crate::auth::oauth_common::parse_query_params;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (mut stream, _) = listener
        .accept()
        .await
        .context("Failed to accept callback")?;
    let mut buffer = vec![0u8; 4096];
    let n = stream
        .read(&mut buffer)
        .await
        .context("Failed to read callback")?;
    let request = String::from_utf8_lossy(&buffer[..n]);

    // Parse GET /callback?code=...&state=...
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query_params(query);

    let code = params
        .get("code")
        .cloned()
        .ok_or_else(|| anyhow!("Callback missing 'code' parameter"))?;
    let state = params
        .get("state")
        .cloned()
        .ok_or_else(|| anyhow!("Callback missing 'state' parameter"))?;

    let html_response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h1>Authenticated!</h1>\
        <p>You can close this window and return to the terminal.</p></body></html>";
    let _ = stream.write_all(html_response.as_bytes()).await;

    Ok((code, state))
}

/// Best-effort browser open (cross-platform).
fn open_browser_url(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(url).spawn();
    }
}

// ── SSE Transport ─────────────────────────────────────────────────────────

/// SSE-based transport (HTTP POST for requests, SSE for responses).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SseStreamState {
    Unknown,
    Connected,
    Unsupported,
}

pub struct SseTransport {
    sse_url: String,
    server_name: String,
    client: reqwest::Client,
    headers: std::collections::HashMap<String, String>,
    stream_state: SseStreamState,
    shared: std::sync::Arc<Mutex<SseSharedState>>,
    notify: std::sync::Arc<Notify>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    reader_task: Option<tokio::task::JoinHandle<()>>,
}

impl SseTransport {
    pub fn new(config: &McpServerConfig) -> Result<Self> {
        let sse_url = config
            .url
            .as_ref()
            .ok_or_else(|| anyhow!("URL required for SSE transport"))?
            .clone();

        let client = reqwest::Client::builder()
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            sse_url,
            server_name: config.name.clone(),
            client,
            headers: config.headers.clone(),
            stream_state: SseStreamState::Unknown,
            shared: std::sync::Arc::new(Mutex::new(SseSharedState::default())),
            notify: std::sync::Arc::new(Notify::new()),
            shutdown_tx: None,
            reader_task: None,
        })
    }

    async fn ensure_connected(&mut self) -> Result<()> {
        if self.stream_state == SseStreamState::Unsupported {
            return Ok(());
        }
        if let Some(task) = &self.reader_task {
            if !task.is_finished() {
                self.stream_state = SseStreamState::Connected;
                return Ok(());
            }
        }

        let has_accept = self
            .headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Accept"));

        let mut req = self
            .client
            .get(&self.sse_url)
            .header("Cache-Control", "no-cache");
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        if !has_accept {
            req = req.header("Accept", MCP_STREAMABLE_ACCEPT);
        }

        let resp = req.send().await.context("SSE GET to MCP server failed")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND
            || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            self.stream_state = SseStreamState::Unsupported;
            return Ok(());
        }
        if !resp.status().is_success() {
            return Err(anyhow!("MCP server returned HTTP {}", resp.status()));
        }
        let is_event_stream = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
        if !is_event_stream {
            self.stream_state = SseStreamState::Unsupported;
            return Ok(());
        }

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        let shared = self.shared.clone();
        let notify = self.notify.clone();
        let sse_url = self.sse_url.clone();
        let server_name = self.server_name.clone();

        self.reader_task = Some(tokio::spawn(async move {
            let stream = resp
                .bytes_stream()
                .map(|item| item.map_err(std::io::Error::other));
            let reader = tokio_util::io::StreamReader::new(stream);
            let mut lines = BufReader::new(reader).lines();

            let mut cur_event: Option<String> = None;
            let mut cur_id: Option<String> = None;
            let mut cur_data: Vec<String> = Vec::new();

            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    line = lines.next_line() => {
                        let Ok(line_opt) = line else { break; };
                        let Some(mut line) = line_opt else { break; };
                        if line.ends_with('\r') {
                            line.pop();
                        }
                        if line.is_empty() {
                            if cur_event.is_none() && cur_id.is_none() && cur_data.is_empty() {
                                continue;
                            }
                            let event = cur_event.take();
                            let data = cur_data.join("\n");
                            cur_data.clear();
                            let id = cur_id.take();
                            handle_sse_event(&server_name, &sse_url, &shared, &notify, event.as_deref(), id.as_deref(), data).await;
                            continue;
                        }

                        if line.starts_with(':') {
                            continue;
                        }

                        if let Some(rest) = line.strip_prefix("event:") {
                            cur_event = Some(rest.trim().to_string());
                        }
                        if let Some(rest) = line.strip_prefix("data:") {
                            let rest = rest.strip_prefix(' ').unwrap_or(rest);
                            cur_data.push(rest.to_string());
                        }
                        if let Some(rest) = line.strip_prefix("id:") {
                            cur_id = Some(rest.trim().to_string());
                        }
                    }
                }
            }

            let pending = {
                let mut guard = shared.lock().await;
                std::mem::take(&mut guard.pending)
            };
            for (_, tx) in pending {
                let _ = tx.send(JsonRpcResponse {
                    jsonrpc: crate::tools::mcp_protocol::JSONRPC_VERSION.to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: INTERNAL_ERROR,
                        message: "SSE connection closed".to_string(),
                        data: None,
                    }),
                });
            }
        }));
        self.stream_state = SseStreamState::Connected;

        Ok(())
    }

    async fn get_message_url(&self) -> Result<(String, bool)> {
        let guard = self.shared.lock().await;
        if let Some(url) = &guard.message_url {
            return Ok((url.clone(), guard.message_url_from_endpoint));
        }
        drop(guard);

        let derived = derive_message_url(&self.sse_url, "messages")
            .or_else(|| derive_message_url(&self.sse_url, "message"))
            .ok_or_else(|| anyhow!("invalid SSE URL"))?;
        let mut guard = self.shared.lock().await;
        if guard.message_url.is_none() {
            guard.message_url = Some(derived.clone());
            guard.message_url_from_endpoint = false;
        }
        Ok((derived, false))
    }

    fn maybe_try_alternate_message_url(
        &self,
        current_url: &str,
        from_endpoint: bool,
    ) -> Option<String> {
        if from_endpoint {
            return None;
        }
        let alt = if current_url.ends_with("/messages") {
            derive_message_url(&self.sse_url, "message")
        } else {
            derive_message_url(&self.sse_url, "messages")
        }?;
        if alt == current_url {
            return None;
        }
        Some(alt)
    }
}

#[derive(Default)]
struct SseSharedState {
    message_url: Option<String>,
    message_url_from_endpoint: bool,
    pending: std::collections::HashMap<u64, oneshot::Sender<JsonRpcResponse>>,
}

fn derive_message_url(sse_url: &str, message_path: &str) -> Option<String> {
    let url = reqwest::Url::parse(sse_url).ok()?;
    let mut segments: Vec<&str> = url.path_segments()?.collect();
    if segments.is_empty() {
        return None;
    }
    if segments.last().copied() == Some("sse") {
        segments.pop();
        segments.push(message_path);
        let mut new_url = url.clone();
        new_url.set_path(&format!("/{}", segments.join("/")));
        return Some(new_url.to_string());
    }
    let mut new_url = url.clone();
    let mut path = url.path().trim_end_matches('/').to_string();
    path.push('/');
    path.push_str(message_path);
    new_url.set_path(&path);
    Some(new_url.to_string())
}

async fn handle_sse_event(
    server_name: &str,
    sse_url: &str,
    shared: &std::sync::Arc<Mutex<SseSharedState>>,
    notify: &std::sync::Arc<Notify>,
    event: Option<&str>,
    _id: Option<&str>,
    data: String,
) {
    let event = event.unwrap_or("message");
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return;
    }

    if event.eq_ignore_ascii_case("endpoint") || event.eq_ignore_ascii_case("mcp-endpoint") {
        if let Some(url) = parse_endpoint_from_data(sse_url, trimmed) {
            let mut guard = shared.lock().await;
            guard.message_url = Some(url);
            guard.message_url_from_endpoint = true;
            drop(guard);
            notify.notify_waiters();
        }
        return;
    }

    if !event.eq_ignore_ascii_case("message") {
        return;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return;
    };

    let Ok(resp) = serde_json::from_value::<JsonRpcResponse>(value.clone()) else {
        let _ = serde_json::from_value::<JsonRpcRequest>(value);
        return;
    };

    let Some(id_val) = resp.id.clone() else {
        return;
    };
    let id = match id_val.as_u64() {
        Some(v) => v,
        None => return,
    };

    let tx = {
        let mut guard = shared.lock().await;
        guard.pending.remove(&id)
    };
    if let Some(tx) = tx {
        let _ = tx.send(resp);
    } else {
        tracing::debug!(
            "MCP SSE `{}` received response for unknown id {}",
            server_name,
            id
        );
    }
}

fn parse_endpoint_from_data(sse_url: &str, data: &str) -> Option<String> {
    if data.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(data).ok()?;
        let endpoint = v.get("endpoint")?.as_str()?;
        return parse_endpoint_from_data(sse_url, endpoint);
    }
    if data.starts_with("http://") || data.starts_with("https://") {
        return Some(data.to_string());
    }
    let base = reqwest::Url::parse(sse_url).ok()?;
    base.join(data).ok().map(|u| u.to_string())
}

fn extract_json_from_sse_text(resp_text: &str) -> Cow<'_, str> {
    let text = resp_text.trim_start_matches('\u{feff}');
    let mut current_data_lines: Vec<&str> = Vec::new();
    let mut last_event_data_lines: Vec<&str> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r').trim_start();
        if line.is_empty() {
            if !current_data_lines.is_empty() {
                last_event_data_lines = std::mem::take(&mut current_data_lines);
            }
            continue;
        }

        if line.starts_with(':') {
            continue;
        }

        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            current_data_lines.push(rest);
        }
    }

    if !current_data_lines.is_empty() {
        last_event_data_lines = current_data_lines;
    }

    if last_event_data_lines.is_empty() {
        return Cow::Borrowed(text.trim());
    }

    if last_event_data_lines.len() == 1 {
        return Cow::Borrowed(last_event_data_lines[0].trim());
    }

    let joined = last_event_data_lines.join("\n");
    Cow::Owned(joined.trim().to_string())
}

fn parse_jsonrpc_response_text(resp_text: &str) -> Result<JsonRpcResponse> {
    let trimmed = resp_text.trim();
    if trimmed.is_empty() {
        bail!("MCP server returned no response");
    }

    let json_text = if looks_like_sse_text(trimmed) {
        extract_json_from_sse_text(trimmed)
    } else {
        Cow::Borrowed(trimmed)
    };

    let mcp_resp: JsonRpcResponse = serde_json::from_str(json_text.as_ref())
        .with_context(|| format!("invalid JSON-RPC response: {}", resp_text))?;
    Ok(mcp_resp)
}

fn looks_like_sse_text(text: &str) -> bool {
    text.starts_with("data:")
        || text.starts_with("event:")
        || text.contains("\ndata:")
        || text.contains("\nevent:")
}

async fn read_first_jsonrpc_from_sse_response(
    resp: reqwest::Response,
) -> Result<Option<JsonRpcResponse>> {
    let stream = resp
        .bytes_stream()
        .map(|item| item.map_err(std::io::Error::other));
    let reader = tokio_util::io::StreamReader::new(stream);
    let mut lines = BufReader::new(reader).lines();

    let mut cur_event: Option<String> = None;
    let mut cur_data: Vec<String> = Vec::new();

    while let Ok(line_opt) = lines.next_line().await {
        let Some(mut line) = line_opt else { break };
        if line.ends_with('\r') {
            line.pop();
        }
        if line.is_empty() {
            if cur_event.is_none() && cur_data.is_empty() {
                continue;
            }
            let event = cur_event.take();
            let data = cur_data.join("\n");
            cur_data.clear();

            let event = event.unwrap_or_else(|| "message".to_string());
            if event.eq_ignore_ascii_case("endpoint") || event.eq_ignore_ascii_case("mcp-endpoint")
            {
                continue;
            }
            if !event.eq_ignore_ascii_case("message") {
                continue;
            }

            let trimmed = data.trim();
            if trimmed.is_empty() {
                continue;
            }
            let json_str = extract_json_from_sse_text(trimmed);
            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(json_str.as_ref()) {
                return Ok(Some(resp));
            }
            continue;
        }

        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            cur_event = Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            cur_data.push(rest.to_string());
        }
    }

    Ok(None)
}

#[async_trait::async_trait]
impl McpTransportConn for SseTransport {
    async fn send_and_recv(&mut self, request: &JsonRpcRequest) -> Result<JsonRpcResponse> {
        self.ensure_connected().await?;

        let id = request.id.as_ref().and_then(|v| v.as_u64());
        let body = serde_json::to_string(request)?;

        let (mut message_url, mut from_endpoint) = self.get_message_url().await?;
        if self.stream_state == SseStreamState::Connected && !from_endpoint {
            for _ in 0..3 {
                {
                    let guard = self.shared.lock().await;
                    if guard.message_url_from_endpoint {
                        if let Some(url) = &guard.message_url {
                            message_url = url.clone();
                            from_endpoint = true;
                            break;
                        }
                    }
                }
                let _ = timeout(Duration::from_millis(300), self.notify.notified()).await;
            }
        }
        let primary_url = if from_endpoint {
            message_url.clone()
        } else {
            self.sse_url.clone()
        };
        let secondary_url = if message_url == self.sse_url {
            None
        } else if primary_url == message_url {
            Some(self.sse_url.clone())
        } else {
            Some(message_url.clone())
        };
        let has_secondary = secondary_url.is_some();

        let mut rx = None;
        if let Some(id) = id {
            if self.stream_state == SseStreamState::Connected {
                let (tx, ch) = oneshot::channel();
                {
                    let mut guard = self.shared.lock().await;
                    guard.pending.insert(id, tx);
                }
                rx = Some((id, ch));
            }
        }

        let mut got_direct = None;
        let mut last_status = None;

        for (i, url) in std::iter::once(primary_url)
            .chain(secondary_url.into_iter())
            .enumerate()
        {
            let has_accept = self
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("Accept"));
            let has_content_type = self
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("Content-Type"));
            let mut req = self
                .client
                .post(&url)
                .timeout(Duration::from_secs(120))
                .body(body.clone());
            if !has_content_type {
                req = req.header("Content-Type", MCP_JSON_CONTENT_TYPE);
            }
            for (key, value) in &self.headers {
                req = req.header(key, value);
            }
            if !has_accept {
                req = req.header("Accept", MCP_STREAMABLE_ACCEPT);
            }

            let resp = req.send().await.context("SSE POST to MCP server failed")?;
            let status = resp.status();
            last_status = Some(status);

            if (status == reqwest::StatusCode::NOT_FOUND
                || status == reqwest::StatusCode::METHOD_NOT_ALLOWED)
                && i == 0
            {
                continue;
            }

            if !status.is_success() {
                break;
            }

            if request.id.is_none() {
                got_direct = Some(JsonRpcResponse {
                    jsonrpc: crate::tools::mcp_protocol::JSONRPC_VERSION.to_string(),
                    id: None,
                    result: None,
                    error: None,
                });
                break;
            }

            let is_sse = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));

            if is_sse {
                if i == 0 && has_secondary {
                    match timeout(
                        Duration::from_secs(3),
                        read_first_jsonrpc_from_sse_response(resp),
                    )
                    .await
                    {
                        Ok(res) => {
                            if let Some(resp) = res? {
                                got_direct = Some(resp);
                            }
                            break;
                        }
                        Err(_) => continue,
                    }
                }
                if let Some(resp) = read_first_jsonrpc_from_sse_response(resp).await? {
                    got_direct = Some(resp);
                }
                break;
            }

            let text = if i == 0 && has_secondary {
                match timeout(Duration::from_secs(3), resp.text()).await {
                    Ok(Ok(t)) => t,
                    Ok(Err(_)) => String::new(),
                    Err(_) => continue,
                }
            } else {
                resp.text().await.unwrap_or_default()
            };
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let json_str = if trimmed.contains("\ndata:") || trimmed.starts_with("data:") {
                    extract_json_from_sse_text(trimmed)
                } else {
                    Cow::Borrowed(trimmed)
                };
                if let Ok(mcp_resp) = serde_json::from_str::<JsonRpcResponse>(json_str.as_ref()) {
                    got_direct = Some(mcp_resp);
                }
            }
            break;
        }

        if let Some((id, _)) = rx.as_ref() {
            if got_direct.is_some() {
                let mut guard = self.shared.lock().await;
                guard.pending.remove(id);
            } else if let Some(status) = last_status {
                if !status.is_success() {
                    let mut guard = self.shared.lock().await;
                    guard.pending.remove(id);
                }
            }
        }

        if let Some(resp) = got_direct {
            return Ok(resp);
        }

        if let Some(status) = last_status {
            if !status.is_success() {
                bail!("MCP server returned HTTP {}", status);
            }
        } else {
            bail!("MCP request not sent");
        }

        let Some((_id, rx)) = rx else {
            bail!("MCP server returned no response");
        };

        rx.await.map_err(|_| anyhow!("SSE response channel closed"))
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        Ok(())
    }
}

// ── Factory ──────────────────────────────────────────────────────────────

/// Create a transport based on config.
pub fn create_transport(config: &McpServerConfig) -> Result<Box<dyn McpTransportConn>> {
    match config.transport {
        McpTransport::Stdio => Ok(Box::new(StdioTransport::new(config)?)),
        McpTransport::Http => Ok(Box::new(HttpTransport::new(config)?)),
        McpTransport::Sse => Ok(Box::new(SseTransport::new(config)?)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_default_is_stdio() {
        let config = McpServerConfig::default();
        assert_eq!(config.transport, McpTransport::Stdio);
    }

    #[test]
    fn test_http_transport_requires_url() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: McpTransport::Http,
            ..Default::default()
        };
        assert!(HttpTransport::new(&config).is_err());
    }

    #[test]
    fn test_sse_transport_requires_url() {
        let config = McpServerConfig {
            name: "test".into(),
            transport: McpTransport::Sse,
            ..Default::default()
        };
        assert!(SseTransport::new(&config).is_err());
    }

    #[test]
    fn test_extract_json_from_sse_data_no_space() {
        let input = "data:{\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_with_event_and_id() {
        let input = "id: 1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_multiline_data() {
        let input = "event: message\ndata: {\ndata:   \"jsonrpc\": \"2.0\",\ndata:   \"result\": {}\ndata: }\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_skips_bom_and_leading_whitespace() {
        let input = "\u{feff}\n\n  data: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_extract_json_from_sse_uses_last_event_with_data() {
        let input =
            ": keep-alive\n\nid: 1\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"result\":{}}\n\n";
        let extracted = extract_json_from_sse_text(input);
        let _: JsonRpcResponse = serde_json::from_str(extracted.as_ref()).unwrap();
    }

    #[test]
    fn test_parse_jsonrpc_response_text_handles_plain_json() {
        let parsed = parse_jsonrpc_response_text("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}")
            .expect("plain JSON response should parse");
        assert_eq!(parsed.id, Some(serde_json::json!(1)));
        assert!(parsed.error.is_none());
    }

    #[test]
    fn test_parse_jsonrpc_response_text_handles_sse_framed_json() {
        let sse =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        let parsed =
            parse_jsonrpc_response_text(sse).expect("SSE-framed JSON response should parse");
        assert_eq!(parsed.id, Some(serde_json::json!(2)));
        assert_eq!(
            parsed
                .result
                .as_ref()
                .and_then(|v| v.get("ok"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn test_parse_jsonrpc_response_text_rejects_empty_payload() {
        assert!(parse_jsonrpc_response_text(" \n\t ").is_err());
    }
}
