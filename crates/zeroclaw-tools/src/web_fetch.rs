use crate::helpers::domain_guard;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::FirecrawlConfig;

/// Minimum body length to consider a standard fetch successful.
/// Bodies shorter than this are treated as JS-only pages that need Firecrawl.
const FIRECRAWL_MIN_BODY_LEN: usize = 100;

/// Web fetch tool: fetches a web page and converts HTML to plain text for LLM consumption.
///
/// Unlike `http_request` (an API client returning raw responses), this tool:
/// - Only supports GET
/// - Follows redirects (up to 10)
/// - Converts HTML to clean plain text via `nanohtml2text`
/// - Passes through text/plain, text/markdown, and application/json as-is
/// - Sets a descriptive User-Agent
/// - Falls back to Firecrawl API when standard fetch fails (if enabled)
pub struct WebFetchTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    allowed_private_hosts: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
    firecrawl: FirecrawlConfig,
}

impl WebFetchTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
        firecrawl: FirecrawlConfig,
        allowed_private_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            allowed_domains: domain_guard::normalize_allowed_domains(
                allowed_domains,
                "web_fetch.allowed_domains",
            )?,
            blocked_domains: domain_guard::normalize_allowed_domains(
                blocked_domains,
                "web_fetch.blocked_domains",
            )?,
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                allowed_private_hosts,
                "web_fetch.allowed_private_hosts",
            )?,
            max_response_size,
            timeout_secs,
            firecrawl,
        })
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        validate_target_url(
            raw_url,
            &self.allowed_domains,
            &self.blocked_domains,
            &self.allowed_private_hosts,
            "web_fetch",
        )
    }

    fn truncate_response(&self, text: &str) -> String {
        // max_response_size == 0 means "unlimited" (matches the
        // http_request tool's documented semantics + tests at
        // crates/zeroclaw-tools/src/http_request.rs:151). Without this
        // branch, the unsigned-arithmetic path below would truncate
        // every response to zero bytes, then append the truncation
        // marker — useless content + spurious Firecrawl fallback.
        if self.max_response_size == 0 {
            return text.to_string();
        }
        if text.len() > self.max_response_size {
            let mut truncated = text
                .chars()
                .take(self.max_response_size)
                .collect::<String>();
            truncated.push_str("\n\n... [Response truncated due to size limit] ...");
            truncated
        } else {
            text.to_string()
        }
    }

    async fn read_response_text_limited(
        &self,
        response: reqwest::Response,
    ) -> anyhow::Result<String> {
        let mut bytes_stream = response.bytes_stream();
        // max_response_size == 0 → unlimited. Without this branch, the
        // existing saturating_add(1) made hard_cap = 1 byte, so the
        // entire stream was truncated after one byte. Use usize::MAX as
        // the effective hard_cap when unlimited so append_chunk_with_cap
        // never stops early on size grounds.
        let hard_cap = if self.max_response_size == 0 {
            usize::MAX
        } else {
            self.max_response_size.saturating_add(1)
        };
        let mut bytes = Vec::new();

        while let Some(chunk_result) = bytes_stream.next().await {
            let chunk = chunk_result?;
            if append_chunk_with_cap(&mut bytes, &chunk, hard_cap) {
                break;
            }
        }

        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Whether the standard fetch result should trigger a Firecrawl fallback.
    fn should_fallback_to_firecrawl(&self, result: &ToolResult) -> bool {
        if !self.firecrawl.enabled {
            return false;
        }
        // Fallback on failure (HTTP error, network error, etc.)
        if !result.success {
            return true;
        }
        // Fallback on empty or very short body (JS-only pages)
        if result.output.trim().len() < FIRECRAWL_MIN_BODY_LEN {
            return true;
        }
        false
    }

    /// Fetch content via the Firecrawl API.
    async fn fetch_via_firecrawl(&self, url: &str) -> anyhow::Result<ToolResult> {
        let api_key = std::env::var(&self.firecrawl.api_key_env).map_err(|_| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "env_var": &self.firecrawl.api_key_env,
                    })),
                "web_fetch: Firecrawl API key missing from env"
            );
            anyhow::Error::msg(format!(
                "Firecrawl API key not found in environment variable '{}'",
                self.firecrawl.api_key_env
            ))
        })?;

        let endpoint = format!("{}/scrape", self.firecrawl.api_url.trim_end_matches('/'));

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "web_fetch: failed to build Firecrawl HTTP client"
                );
                anyhow::Error::msg(format!("Failed to build Firecrawl HTTP client: {e}"))
            })?;

        let body = json!({
            "url": url,
            "formats": ["markdown"]
        });

        let response = client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "firecrawl_request",
                            "error": format!("{}", e),
                        })),
                    "web_fetch: Firecrawl request failed"
                );
                anyhow::Error::msg(format!("Firecrawl request failed: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Firecrawl API error: HTTP {} - {}",
                    status.as_u16(),
                    error_body
                )),
            });
        }

        let resp_json: serde_json::Value = response.json().await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "firecrawl_response_parse",
                        "error": format!("{}", e),
                    })),
                "web_fetch: failed to parse Firecrawl response"
            );
            anyhow::Error::msg(format!("Failed to parse Firecrawl response: {e}"))
        })?;

        let markdown = resp_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");

        if markdown.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Firecrawl returned empty markdown content".into()),
            });
        }

        let output = self.truncate_response(markdown);

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    /// Perform the standard HTTP GET fetch and convert to text.
    async fn standard_fetch(&self, client: &reqwest::Client, url: &str) -> ToolResult {
        let response = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("HTTP request failed: {e}")),
                };
            }
        };

        let status = response.status();
        if !status.is_success() {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "HTTP {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Unknown")
                )),
            };
        }

        // Determine content type for processing strategy
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body_mode = if content_type.contains("text/html") || content_type.is_empty() {
            "html"
        } else if content_type.contains("text/plain")
            || content_type.contains("text/markdown")
            || content_type.contains("application/json")
        {
            "plain"
        } else {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported content type: {content_type}. \
                     web_fetch supports text/html, text/plain, text/markdown, and application/json."
                )),
            };
        };

        let body = match self.read_response_text_limited(response).await {
            Ok(t) => t,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read response body: {e}")),
                };
            }
        };

        let text = if body_mode == "html" {
            nanohtml2text::html2text(&body)
        } else {
            body
        };

        let output = self.truncate_response(&text);

        ToolResult {
            success: true,
            output,
            error: None,
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a web page and return its content as clean plain text. \
         HTML pages are automatically converted to readable text. \
         JSON and plain text responses are returned as-is. \
         Only GET requests; follows redirects. \
         Falls back to Firecrawl for JS-heavy/bot-blocked sites (if enabled). \
         Security: allowlist-only domains, no local/private hosts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "url"})),
                "web_fetch: missing url parameter"
            );
            anyhow::Error::msg("Missing 'url' parameter")
        })?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        let url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        // Build client: follow redirects, set timeout, set User-Agent
        let timeout_secs = if self.timeout_secs == 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "web_fetch: timeout_secs is 0, using safe default of 30s"
            );
            30
        } else {
            self.timeout_secs
        };

        let allowed_domains = self.allowed_domains.clone();
        let blocked_domains = self.blocked_domains.clone();
        let allowed_private_hosts = self.allowed_private_hosts.clone();
        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error(std::io::Error::other("Too many redirects (max 10)"));
            }

            if let Err(err) = validate_target_url(
                attempt.url().as_str(),
                &allowed_domains,
                &blocked_domains,
                &allowed_private_hosts,
                "web_fetch",
            ) {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Blocked redirect target: {err}"),
                ));
            }

            attempt.follow()
        });

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(redirect_policy)
            .user_agent("ZeroClaw/0.1 (web_fetch)");
        let builder =
            zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_fetch");
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                });
            }
        };

        let standard_result = self.standard_fetch(&client, &url).await;

        // If standard fetch succeeded well enough, return it directly.
        // Otherwise, try Firecrawl fallback if enabled.
        if self.should_fallback_to_firecrawl(&standard_result) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"url": url})),
                "web_fetch: standard fetch insufficient for , attempting Firecrawl fallback"
            );
            match Box::pin(self.fetch_via_firecrawl(&url)).await {
                Ok(firecrawl_result) if firecrawl_result.success => {
                    return Ok(firecrawl_result);
                }
                Ok(firecrawl_result) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "web_fetch: Firecrawl fallback also failed: {:?}",
                            firecrawl_result.error
                        )
                    );
                    // Return original standard result if Firecrawl also failed
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "web_fetch: Firecrawl fallback error"
                    );
                }
            }
        }

        Ok(standard_result)
    }
}

// ── Helper functions (independent from http_request.rs per DRY rule-of-three) ──

fn validate_target_url(
    raw_url: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    allowed_private_hosts: &[String],
    tool_name: &str,
) -> anyhow::Result<String> {
    validate_target_url_with_dns_check(
        raw_url,
        allowed_domains,
        blocked_domains,
        allowed_private_hosts,
        tool_name,
        validate_resolved_host_is_public,
    )
}

fn validate_target_url_with_dns_check(
    raw_url: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    allowed_private_hosts: &[String],
    tool_name: &str,
    validate_dns: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<String> {
    let url = raw_url.trim();

    if url.is_empty() {
        anyhow::bail!("URL cannot be empty");
    }

    if url.chars().any(char::is_whitespace) {
        anyhow::bail!("URL cannot contain whitespace");
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("Only http:// and https:// URLs are allowed");
    }

    if allowed_domains.is_empty() {
        anyhow::bail!(
            "{tool_name} tool is enabled but no allowed_domains are configured. \
             Add [{tool_name}].allowed_domains in config.toml"
        );
    }

    let host = extract_host(url)?;

    // blocked_domains always takes precedence
    if domain_guard::host_matches_allowlist(&host, blocked_domains) {
        anyhow::bail!("Host '{host}' is in {tool_name}.blocked_domains");
    }

    let host_is_private_or_local = domain_guard::is_private_or_local_host(&host);
    let private_match = private_allowlist_match(&host, allowed_private_hosts);
    // An explicit entry (a specific host/IP or suffix) is a deliberate per-host
    // carve-out; the "*" wildcard blanket-tolerates a private/internal
    // resolution for any host. The distinction only affects the WARN below.
    let private_explicit = matches!(private_match, PrivateAllow::Explicit);
    // Either an explicit entry or "*" tolerates a private/internal host: it lifts
    // the literal private-host block and skips the resolved-IP public check.
    let private_tolerated = !matches!(private_match, PrivateAllow::None);

    if host_is_private_or_local && !private_tolerated {
        anyhow::bail!(
            "Blocked local/private host: {host}. \
             To allow this host, add it (or \"*\") to \
             {tool_name}.allowed_private_hosts in config.toml"
        );
    }

    // Only WARN when a private-host bypass concretely fired here: an explicit
    // carve-out, or "*" lifting the block for a literally private/local host.
    // Gating on bare `private_tolerated` would log this SSRF-bypass on *every*
    // fetch once "*" is set, burying the real signal. (The new "*"-tolerates-a-
    // resolved-private-IP case is only knowable post-DNS, which this path skips.)
    if private_explicit || (private_tolerated && host_is_private_or_local) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"tool_name": tool_name, "host": host})),
            "web_fetch: allowing host via allowed_private_hosts"
        );
    }

    // The allowed_domains check is skipped only for a host that is *literally*
    // private/local and covered by the private allowlist (an explicit entry, or
    // "*"). A non-private host — including an explicit internal DNS name or any
    // host reached via "*" — still requires allowed_domains approval, so the
    // private allowlist can never be used to reach an arbitrary public host.
    let skip_allowed_domains = host_is_private_or_local && private_tolerated;

    if !skip_allowed_domains && !domain_guard::host_matches_allowlist(&host, allowed_domains) {
        anyhow::bail!("Host '{host}' is not in {tool_name}.allowed_domains");
    }

    // Skip the resolved-IP public check only when the host is covered by the
    // private allowlist (explicit OR "*"). This is what lets a domain that
    // resolves to a private IP through under allowed_private_hosts = ["*"].
    if !private_tolerated {
        validate_dns(&host)?;
    }

    Ok(url.to_string())
}

fn append_chunk_with_cap(buffer: &mut Vec<u8>, chunk: &[u8], hard_cap: usize) -> bool {
    if buffer.len() >= hard_cap {
        return true;
    }

    let remaining = hard_cap - buffer.len();
    if chunk.len() > remaining {
        buffer.extend_from_slice(&chunk[..remaining]);
        return true;
    }

    buffer.extend_from_slice(chunk);
    buffer.len() >= hard_cap
}

fn extract_host(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"url": url})),
                "web_fetch: non-http(s) URL rejected"
            );
            anyhow::Error::msg("Only http:// and https:// URLs are allowed")
        })?;

    let authority = rest.split(['/', '?', '#']).next().ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"url": url})),
            "web_fetch: invalid URL"
        );
        anyhow::Error::msg("Invalid URL")
    })?;

    if authority.is_empty() {
        anyhow::bail!("URL must include a host");
    }

    if authority.contains('@') {
        anyhow::bail!("URL userinfo is not allowed");
    }

    if authority.starts_with('[') {
        anyhow::bail!("IPv6 hosts are not supported in web_fetch");
    }

    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.')
        .to_lowercase();

    if host.is_empty() {
        anyhow::bail!("URL must include a valid host");
    }

    Ok(host)
}

/// How a host is covered by `allowed_private_hosts`.
///
/// The distinction only affects logging: an explicit entry is a deliberate
/// per-host carve-out, whereas `*` blanket-tolerates a private/internal
/// resolution. Both lift the literal private-host block and skip the
/// resolved-IP public check; neither widens `allowed_domains` for a non-private
/// host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateAllow {
    /// Not covered by the private allowlist.
    None,
    /// Covered only by a `*` wildcard entry.
    Wildcard,
    /// Covered by a specific host/IP or suffix entry.
    Explicit,
}

fn private_allowlist_match(host: &str, allowed_private_hosts: &[String]) -> PrivateAllow {
    let mut wildcard = false;
    for entry in allowed_private_hosts {
        if entry == "*" {
            // Record the wildcard but keep scanning: a later explicit entry
            // should still win, since it is a deliberate per-host carve-out.
            wildcard = true;
        } else if domain_guard::host_matches_allowlist(host, std::slice::from_ref(entry)) {
            return PrivateAllow::Explicit;
        }
    }
    if wildcard {
        PrivateAllow::Wildcard
    } else {
        PrivateAllow::None
    }
}

#[cfg(not(test))]
fn validate_resolved_host_is_public(host: &str) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    let ips = (host, 0)
        .to_socket_addrs()
        .map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "host": host,
                        "error": format!("{}", e),
                    })),
                "web_fetch: failed to resolve host"
            );
            anyhow::Error::msg(format!("Failed to resolve host '{host}': {e}"))
        })?
        .map(|addr| addr.ip())
        .collect::<Vec<_>>();

    validate_resolved_ips_are_public(host, &ips)
}

#[cfg(test)]
fn validate_resolved_host_is_public(_host: &str) -> anyhow::Result<()> {
    // DNS checks are covered by validate_resolved_ips_are_public unit tests.
    Ok(())
}

fn validate_resolved_ips_are_public(host: &str, ips: &[std::net::IpAddr]) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        let non_global = match ip {
            std::net::IpAddr::V4(v4) => domain_guard::is_non_global_v4(*v4),
            std::net::IpAddr::V6(v6) => domain_guard::is_non_global_v6(*v6),
        };
        if non_global {
            anyhow::bail!(
                "Blocked host '{host}' resolved to non-global address {ip}. \
                 To allow hosts that resolve to private/internal IPs, add '{host}' \
                 (or \"*\") to web_fetch.allowed_private_hosts in config.toml"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::FirecrawlConfig;

    fn test_tool(allowed_domains: Vec<&str>) -> WebFetchTool {
        test_tool_with_blocklist(allowed_domains, vec![])
    }

    fn test_tool_with_blocklist(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            blocked_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap()
    }

    fn test_tool_with_private_hosts(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
        allowed_private_hosts: Vec<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            blocked_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
            FirecrawlConfig::default(),
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .unwrap()
    }

    fn test_tool_with_firecrawl(firecrawl: FirecrawlConfig) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            firecrawl,
            vec![],
        )
        .unwrap()
    }

    // ── Name and schema ──────────────────────────────────────────

    #[test]
    fn name_is_web_fetch() {
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "web_fetch");
    }

    #[test]
    fn parameters_schema_requires_url() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    // ── HTML to text conversion ──────────────────────────────────

    #[test]
    fn html_to_text_conversion() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = nanohtml2text::html2text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("<p>"));
    }

    // ── URL validation ───────────────────────────────────────────

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/page").unwrap();
        assert_eq!(got, "https://example.com/page");
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://docs.example.com/guide").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://news.ycombinator.com").is_ok());
    }

    #[test]
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_missing_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("  ").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_ftp_scheme() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
    }

    #[test]
    fn validate_rejects_allowlist_miss() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://google.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_requires_allowlist() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = WebFetchTool::new(
            security,
            vec![],
            vec![],
            500_000,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    // ── SSRF protection ──────────────────────────────────────────

    #[test]
    fn ssrf_blocks_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_blocks_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_wildcard_still_blocks_private() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn redirect_target_validation_allows_permitted_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked = vec![];
        assert!(
            validate_target_url(
                "https://docs.example.com/page",
                &allowed,
                &blocked,
                &[],
                "web_fetch"
            )
            .is_ok()
        );
    }

    #[test]
    fn redirect_target_validation_blocks_private_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked = vec![];
        let err = validate_target_url(
            "https://127.0.0.1/admin",
            &allowed,
            &blocked,
            &[],
            "web_fetch",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn redirect_target_validation_blocks_blocklisted_host() {
        let allowed = vec!["*".to_string()];
        let blocked = vec!["evil.com".to_string()];
        let err = validate_target_url(
            "https://evil.com/phish",
            &allowed,
            &blocked,
            &[],
            "web_fetch",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("blocked_domains"));
    }

    // ── Security policy ──────────────────────────────────────────

    #[tokio::test]
    async fn blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["example.com".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    // ── Response truncation ──────────────────────────────────────

    #[test]
    fn truncate_within_limit() {
        let tool = test_tool(vec!["example.com"]);
        let text = "hello world";
        assert_eq!(tool.truncate_response(text), "hello world");
    }

    #[test]
    fn truncate_response_zero_means_unlimited() {
        // max_response_size == 0 must be treated as unlimited — no truncation
        // marker, full text returned regardless of length.
        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            vec![],
            0, // unlimited
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let long_text = "x".repeat(10_000);
        let result = tool.truncate_response(&long_text);
        assert_eq!(result.len(), 10_000, "zero limit must not truncate");
        assert!(
            !result.contains("[Response truncated"),
            "must not append truncation marker"
        );
    }

    /// Drives the actual streamed-read path (standard_fetch +
    /// read_response_text_limited) via wiremock to lock in the
    /// max_response_size=0 behaviour. Audacity88 review (PR #6884)
    /// flagged the direct-helper test as insufficient because it
    /// did not exercise the saturating_add(1) cap that previously
    /// stopped streaming after 1 byte and triggered spurious
    /// Firecrawl fallback.
    #[tokio::test]
    async fn standard_fetch_with_zero_limit_returns_full_body_and_skips_firecrawl_fallback() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let addr = server.address();

        // Body must exceed FIRECRAWL_MIN_BODY_LEN (100 bytes) so any
        // truncation to <100 bytes would (incorrectly) trigger fallback.
        let body = "a".repeat(500);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body.clone()))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Supervised,
                ..SecurityPolicy::default()
            }),
            vec!["*".into()],
            vec![],
            0, // max_response_size = unlimited
            30,
            FirecrawlConfig {
                enabled: true,
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        // Bypass SSRF-guarded execute() — call standard_fetch directly so
        // wiremock on 127.0.0.1 is reachable.
        let url = format!("http://{}:{}/", addr.ip(), addr.port());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("reqwest client");
        let standard_result = tool.standard_fetch(&client, &url).await;

        // (a) standard result IS the full body — proves streamed read did
        // not stop after 1 byte under the zero-limit path.
        assert!(
            standard_result.success,
            "standard_fetch must succeed, got error={:?}",
            standard_result.error
        );
        assert_eq!(
            standard_result.output.len(),
            body.len(),
            "streamed body length under zero-limit must equal full body"
        );
        assert_eq!(
            standard_result.output, body,
            "streamed body content must equal full body"
        );
        assert!(
            !standard_result.output.contains("[Response truncated"),
            "must not append truncation marker under zero limit"
        );

        // (b) result does NOT trip should_fallback_to_firecrawl — proves
        // the regression (1-byte short body) is locked out.
        assert!(
            !tool.should_fallback_to_firecrawl(&standard_result),
            "500-byte body under zero limit must not trigger Firecrawl fallback"
        );
    }

    #[test]
    fn truncate_over_limit() {
        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            vec![],
            10,
            30,
            FirecrawlConfig::default(),
            vec![],
        )
        .unwrap();
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.contains("[Response truncated"));
    }

    // ── Domain normalization ─────────────────────────────────────
    // ── Blocked domains ──────────────────────────────────────────

    #[test]
    fn blocklist_rejects_exact_match() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com/page")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_rejects_subdomain() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://api.evil.com/v1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_wins_over_allowlist() {
        let tool = test_tool_with_blocklist(vec!["evil.com"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_allows_non_blocked() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        assert!(tool.validate_url("https://example.com").is_ok());
    }

    #[test]
    fn append_chunk_with_cap_truncates_and_stops() {
        let mut buffer = Vec::new();
        assert!(!append_chunk_with_cap(&mut buffer, b"hello", 8));
        assert!(append_chunk_with_cap(&mut buffer, b"world", 8));
        assert_eq!(buffer, b"hellowor");
    }

    #[test]
    fn resolved_private_ip_is_rejected() {
        let ips = vec!["127.0.0.1".parse().unwrap()];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-global address"));
    }

    #[test]
    fn resolved_mixed_ips_are_rejected() {
        let ips = vec![
            "93.184.216.34".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
        ];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(err.contains("non-global address"));
    }

    #[test]
    fn resolved_public_ips_are_allowed() {
        let ips = vec!["93.184.216.34".parse().unwrap(), "1.1.1.1".parse().unwrap()];
        assert!(validate_resolved_ips_are_public("example.com", &ips).is_ok());
    }

    // ── Firecrawl config parsing ────────────────────────────────────

    #[test]
    fn firecrawl_config_defaults() {
        let cfg = FirecrawlConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.api_key_env, "FIRECRAWL_API_KEY");
        assert_eq!(cfg.api_url, "https://api.firecrawl.dev/v1");
        assert_eq!(cfg.mode, zeroclaw_config::schema::FirecrawlMode::Scrape);
    }

    #[test]
    fn firecrawl_config_deserializes_from_toml() {
        let toml_str = r#"
            enabled = true
            api_key_env = "MY_FC_KEY"
            api_url = "https://custom.firecrawl.io/v2"
            mode = "crawl"
        "#;
        let cfg: FirecrawlConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.api_key_env, "MY_FC_KEY");
        assert_eq!(cfg.api_url, "https://custom.firecrawl.io/v2");
        assert_eq!(cfg.mode, zeroclaw_config::schema::FirecrawlMode::Crawl);
    }

    #[test]
    fn firecrawl_config_deserializes_defaults_from_empty_toml() {
        let cfg: FirecrawlConfig = toml::from_str("").unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.api_key_env, "FIRECRAWL_API_KEY");
    }

    #[test]
    fn web_fetch_config_with_firecrawl_section() {
        use zeroclaw_config::schema::WebFetchConfig;
        let toml_str = r#"
            enabled = true
            [firecrawl]
            enabled = true
            api_key_env = "FC_KEY"
        "#;
        let cfg: WebFetchConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.firecrawl.enabled);
        assert_eq!(cfg.firecrawl.api_key_env, "FC_KEY");
    }

    // ── Firecrawl fallback trigger conditions ───────────────────────

    #[test]
    fn fallback_disabled_when_firecrawl_not_enabled() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig::default());
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("HTTP 403 Forbidden".into()),
        };
        assert!(!tool.should_fallback_to_firecrawl(&result));
    }

    #[test]
    fn fallback_triggers_on_http_error() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("HTTP 403 Forbidden".into()),
        };
        assert!(tool.should_fallback_to_firecrawl(&result));
    }

    #[test]
    fn fallback_triggers_on_empty_body() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let result = ToolResult {
            success: true,
            output: String::new(),
            error: None,
        };
        assert!(tool.should_fallback_to_firecrawl(&result));
    }

    #[test]
    fn fallback_triggers_on_short_body() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let result = ToolResult {
            success: true,
            output: "Loading...".into(), // < 100 chars, JS-only page
            error: None,
        };
        assert!(tool.should_fallback_to_firecrawl(&result));
    }

    #[test]
    fn fallback_skipped_on_good_response() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let result = ToolResult {
            success: true,
            output: "A".repeat(200), // well above 100 chars
            error: None,
        };
        assert!(!tool.should_fallback_to_firecrawl(&result));
    }

    // ── Firecrawl response parsing ──────────────────────────────────

    #[test]
    fn firecrawl_response_parses_markdown() {
        let response_json = json!({
            "success": true,
            "data": {
                "markdown": "# Hello World\n\nThis is extracted content from Firecrawl.",
                "metadata": {
                    "title": "Test Page"
                }
            }
        });
        let markdown = response_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(markdown.contains("Hello World"));
        assert!(markdown.contains("extracted content"));
    }

    #[test]
    fn firecrawl_response_handles_missing_markdown() {
        let response_json = json!({
            "success": true,
            "data": {}
        });
        let markdown = response_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(markdown.is_empty());
    }

    #[test]
    fn firecrawl_response_handles_missing_data() {
        let response_json = json!({
            "success": false,
            "error": "Rate limit exceeded"
        });
        let markdown = response_json
            .get("data")
            .and_then(|d| d.get("markdown"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert!(markdown.is_empty());
    }

    // ── Boundary test: FIRECRAWL_MIN_BODY_LEN (100 chars) ────────────

    #[test]
    fn fallback_triggers_at_exactly_99_chars() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let result = ToolResult {
            success: true,
            output: "A".repeat(99),
            error: None,
        };
        assert!(
            tool.should_fallback_to_firecrawl(&result),
            "99-char body (below threshold) should trigger fallback"
        );
    }

    #[test]
    fn fallback_skipped_at_exactly_100_chars() {
        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            ..FirecrawlConfig::default()
        });
        let result = ToolResult {
            success: true,
            output: "A".repeat(100),
            error: None,
        };
        assert!(
            !tool.should_fallback_to_firecrawl(&result),
            "100-char body (at threshold) should NOT trigger fallback"
        );
    }

    // ── Item 1: missing API key env var falls back gracefully ─────────

    #[tokio::test]
    async fn firecrawl_missing_api_key_returns_error() {
        // Ensure the env var is unset for this test
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FIRECRAWL_TEST_MISSING_KEY") };

        let tool = test_tool_with_firecrawl(FirecrawlConfig {
            enabled: true,
            api_key_env: "FIRECRAWL_TEST_MISSING_KEY".into(),
            ..FirecrawlConfig::default()
        });

        let result = tool.fetch_via_firecrawl("https://example.com").await;
        assert!(
            result.is_err(),
            "fetch_via_firecrawl should return Err when API key env var is missing"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FIRECRAWL_TEST_MISSING_KEY"),
            "Error should mention the missing env var name, got: {err_msg}"
        );
    }

    // ── Item 2: double-failure returns original standard result ───────

    #[tokio::test]
    async fn execute_double_failure_returns_original_result() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let addr = server.address();

        // Standard fetch returns 403 (failure)
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        // Ensure Firecrawl API key env is missing so fallback also fails
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FIRECRAWL_DOUBLE_FAIL_KEY") };

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig {
                enabled: true,
                api_key_env: "FIRECRAWL_DOUBLE_FAIL_KEY".into(),
                api_url: format!("http://{addr}"),
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        // Bypass SSRF-guarded execute() — call standard_fetch + fallback
        // logic directly so wiremock on 127.0.0.1 is reachable.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        let url = format!("http://{addr}/page");
        let standard_result = tool.standard_fetch(&client, &url).await;

        // standard_fetch should fail with 403
        assert!(!standard_result.success);
        assert!(tool.should_fallback_to_firecrawl(&standard_result));

        // Firecrawl fallback should also fail (missing API key)
        let firecrawl_result = Box::pin(tool.fetch_via_firecrawl(&url)).await;
        assert!(
            firecrawl_result.is_err() || !firecrawl_result.as_ref().unwrap().success,
            "Expected Firecrawl fallback to fail without API key"
        );

        // The orchestration should return the original 403 error
        assert!(
            standard_result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("403"),
            "Expected original HTTP 403 error, got: {:?}",
            standard_result.error
        );
    }

    // ── Item 3: end-to-end fallback orchestration in execute() ───────

    #[tokio::test]
    async fn execute_falls_back_to_firecrawl_on_short_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Standard-fetch server: returns a very short body (JS-only placeholder)
        let standard_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>Loading...</body></html>")
                    .insert_header("content-type", "text/html"),
            )
            .mount(&standard_server)
            .await;

        // Firecrawl server: returns rich markdown content
        let firecrawl_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/scrape"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {
                    "markdown": "# Real Content\n\nThis is the full page content extracted by Firecrawl, with enough text to be clearly above the minimum body length threshold."
                }
            })))
            .mount(&firecrawl_server)
            .await;

        // Set up API key env var for this test
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("FIRECRAWL_E2E_TEST_KEY", "test-key-12345") };

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let standard_addr = standard_server.address();
        let firecrawl_addr = firecrawl_server.address();
        let tool = WebFetchTool::new(
            security,
            vec!["*".into()],
            vec![],
            500_000,
            30,
            FirecrawlConfig {
                enabled: true,
                api_key_env: "FIRECRAWL_E2E_TEST_KEY".into(),
                api_url: format!("http://{firecrawl_addr}"),
                ..FirecrawlConfig::default()
            },
            vec![],
        )
        .unwrap();

        // Bypass SSRF-guarded execute() — call standard_fetch + fallback
        // logic directly so wiremock on 127.0.0.1 is reachable.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        let url = format!("http://{standard_addr}/page");
        let standard_result = tool.standard_fetch(&client, &url).await;

        // Standard fetch returns short body, should trigger fallback
        assert!(tool.should_fallback_to_firecrawl(&standard_result));

        // Firecrawl fallback should succeed with rich content
        let result = Box::pin(tool.fetch_via_firecrawl(&url)).await.unwrap();

        assert!(result.success, "Expected successful Firecrawl fallback");
        assert!(
            result.output.contains("Real Content"),
            "Expected Firecrawl markdown content, got: {}",
            result.output
        );

        // Clean up env var
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("FIRECRAWL_E2E_TEST_KEY") };
    }

    // ── Allowed private hosts ─────────────────────────────────────

    #[test]
    fn allowed_private_host_bypasses_ssrf_block() {
        let tool = test_tool_with_private_hosts(vec!["*"], vec![], vec!["192.168.1.5"]);
        assert!(tool.validate_url("https://192.168.1.5/api").is_ok());
    }

    #[test]
    fn allowed_private_domain_skips_dns_public_check() {
        let allowed_domains = vec!["*".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["local.internal".to_string()];

        let result = validate_target_url_with_dns_check(
            "https://local.internal/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| {
                panic!("DNS public-host validation should be skipped");
            },
        );

        assert!(
            result.is_ok(),
            "allowlisted private domain was rejected: {result:?}"
        );
    }

    #[test]
    fn private_wildcard_allows_domain_resolving_to_private_ip() {
        // Regression for #7412: allowed_private_hosts = ["*"] must permit a
        // regular domain that resolves to a private/internal IP, as long as the
        // name itself passes allowed_domains. The DNS public check must be
        // skipped (closure panics if reached).
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let result = validate_target_url_with_dns_check(
            "https://internal.example.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| panic!("DNS public-host validation should be skipped under private wildcard"),
        );

        assert!(
            result.is_ok(),
            "private wildcard should allow subdomain of allowed_domains: {result:?}"
        );
    }

    #[test]
    fn private_wildcard_allows_literal_private_ip_without_allowed_domains_entry() {
        // The "*" wildcard must keep its historical scope for *literal* private
        // hosts: an IP literal (or localhost/.local) is allowed even when it is
        // not listed in allowed_domains. Only ordinary domain names stay gated
        // on allowed_domains under "*".
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let result = validate_target_url_with_dns_check(
            "https://10.0.0.1/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| panic!("DNS public-host validation should be skipped for a literal private IP"),
        );

        assert!(
            result.is_ok(),
            "private wildcard should allow a literal private IP: {result:?}"
        );
    }

    #[test]
    fn private_allowlist_explicit_entry_must_pass_allowed_domains() {
        // An explicit (non-private) entry in allowed_private_hosts is NOT a free
        // pass: a non-private host still has to be in allowed_domains.
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["unrelated.com".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://unrelated.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("allowed_domains"), "unexpected error: {err}");
    }

    #[test]
    fn private_wildcard_still_requires_allowed_domains() {
        // The "*" private wildcard must NOT widen the name allowlist: a public
        // domain that is not in allowed_domains stays blocked.
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://evil.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("allowed_domains"), "unexpected error: {err}");
    }

    #[test]
    fn unallowed_domain_resolving_private_ip_still_blocked() {
        let allowed_domains = vec!["*".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec![];

        let err = validate_target_url_with_dns_check(
            "https://local.internal/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |host| {
                validate_resolved_ips_are_public(
                    host,
                    &[std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                        192, 168, 1, 5,
                    ))],
                )
            },
        )
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("non-global address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn private_allowlist_wildcard_does_not_allow_public_domain_miss() {
        let allowed_domains = vec!["example.com".to_string()];
        let blocked_domains = vec![];
        let allowed_private_hosts = vec!["*".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://not-example.com/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::Ok(()),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("allowed_domains"), "unexpected error: {err}");
    }

    #[test]
    fn blocklist_overrides_allowed_private_domain() {
        let allowed_domains = vec!["*".to_string()];
        let blocked_domains = vec!["local.internal".to_string()];
        let allowed_private_hosts = vec!["local.internal".to_string()];

        let err = validate_target_url_with_dns_check(
            "https://local.internal/api",
            &allowed_domains,
            &blocked_domains,
            &allowed_private_hosts,
            "web_fetch",
            |_| anyhow::bail!("blocklist should run before DNS validation"),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("blocked_domains"), "unexpected error: {err}");
    }

    #[test]
    fn unallowed_private_host_still_blocked() {
        let tool = test_tool_with_private_hosts(vec!["*"], vec![], vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://10.0.0.1/admin")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
        assert!(err.contains("allowed_private_hosts"));
    }

    #[test]
    fn blocklist_overrides_allowed_private_host() {
        let tool =
            test_tool_with_private_hosts(vec!["*"], vec!["192.168.1.5"], vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5/secret")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn allowed_private_host_with_port() {
        let tool = test_tool_with_private_hosts(vec!["*"], vec![], vec!["192.168.1.5"]);
        assert!(tool.validate_url("https://192.168.1.5:8080/api").is_ok());
    }
}
