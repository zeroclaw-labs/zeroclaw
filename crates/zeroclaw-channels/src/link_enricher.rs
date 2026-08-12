//! Link enricher: auto-detects URLs in inbound messages, fetches their content,
//! and prepends summaries so the agent has link context without explicit tool calls.

use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;

/// Configuration for the link enricher pipeline stage.
#[derive(Debug, Clone)]
pub struct LinkEnricherConfig {
    pub enabled: bool,
    pub max_links: usize,
    pub timeout_secs: u64,
}

impl Default for LinkEnricherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_links: 3,
            timeout_secs: 10,
        }
    }
}

/// URL regex: matches `http://` and `https://` URLs, stopping at whitespace, angle
/// brackets, or double-quotes.
static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"']+"#).expect("URL regex must compile"));

/// Extract URLs from message text, returning up to `max` unique URLs.
pub fn extract_urls(text: &str, max: usize) -> Vec<String> {
    let mut seen = Vec::new();
    for m in URL_RE.find_iter(text) {
        let url = m.as_str().to_string();
        if !seen.contains(&url) {
            seen.push(url);
            if seen.len() >= max {
                break;
            }
        }
    }
    seen
}

/// Returns `true` if the URL points to a private/local address that should be
/// blocked for SSRF protection.
pub fn is_ssrf_target(url: &str) -> bool {
    reqwest::Url::parse(url).map_or(true, |url| is_blocked_url(&url))
}

fn is_blocked_url(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }

    let Some(host) = url.host_str() else {
        return true;
    };
    let canonical_host = host.trim_end_matches('.');
    canonical_host.is_empty() || zeroclaw_infra::net_guard::is_private_or_local_host(canonical_host)
}

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if is_blocked_url(attempt.url()) {
            return attempt.error(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Link-enricher redirect target is not a public HTTP(S) URL",
            ));
        }

        reqwest::redirect::Policy::limited(MAX_REDIRECTS).redirect(attempt)
    })
}

fn http_client_builder(timeout_secs: u64) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(5))
        .redirect(redirect_policy())
        .user_agent("ZeroClaw/0.1 (link-enricher)")
}

fn http_client(timeout_secs: u64) -> reqwest::Result<reqwest::Client> {
    http_client_builder(timeout_secs).build()
}

/// Extract the `<title>` tag content from HTML.
pub fn extract_title(html: &str) -> Option<String> {
    // Case-insensitive search for <title>...</title>
    let lower = html.to_lowercase();
    let start = lower.find("<title")? + "<title".len();
    // Skip attributes if any (e.g. <title lang="en">)
    let start = lower[start..].find('>')? + start + 1;
    let end = lower[start..].find("</title")? + start;
    let title = lower[start..end].trim().to_string();
    if title.is_empty() {
        None
    } else {
        Some(html_entity_decode_basic(&title))
    }
}

/// Extract the first `max_chars` of visible body text from HTML.
pub fn extract_body_text(html: &str, max_chars: usize) -> String {
    let text = nanohtml2text::html2text(html);
    let trimmed = text.trim();
    if trimmed.len() <= max_chars {
        trimmed.to_string()
    } else {
        let mut result: String = trimmed.chars().take(max_chars).collect();
        result.push_str("...");
        result
    }
}

/// Basic HTML entity decoding for title content.
fn html_entity_decode_basic(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Summary of a fetched link.
struct LinkSummary {
    title: String,
    snippet: String,
}

/// Fetch a single URL and extract a summary. Returns `None` on any failure.
async fn fetch_link_summary(client: &reqwest::Client, url: &str) -> Option<LinkSummary> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    // Only process text/html responses
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    if !content_type.contains("text/html") && !content_type.is_empty() {
        return None;
    }

    // Read up to 256KB to extract title and snippet
    let max_bytes: usize = 256 * 1024;
    let bytes = response.bytes().await.ok()?;
    let body = if bytes.len() > max_bytes {
        String::from_utf8_lossy(&bytes[..max_bytes]).into_owned()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };

    let title = extract_title(&body).unwrap_or_else(|| "Untitled".to_string());
    let snippet = extract_body_text(&body, 200);

    Some(LinkSummary { title, snippet })
}

pub async fn enrich_message(content: &str, config: &LinkEnricherConfig) -> String {
    if !config.enabled || config.max_links == 0 {
        return content.to_string();
    }

    let urls = extract_urls(content, config.max_links);
    if urls.is_empty() {
        return content.to_string();
    }

    // Filter out SSRF targets
    let safe_urls: Vec<&str> = urls
        .iter()
        .filter(|u| !is_ssrf_target(u))
        .map(|u| u.as_str())
        .collect();
    if safe_urls.is_empty() {
        return content.to_string();
    }

    let client = match http_client(config.timeout_secs) {
        Ok(client) => client,
        Err(_) => return content.to_string(),
    };

    let mut enrichments = Vec::new();
    for url in safe_urls {
        match fetch_link_summary(&client, url).await {
            Some(summary) => {
                enrichments.push(format!("[Link: {} — {}]", summary.title, summary.snippet));
            }
            None => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"url": url})),
                    "Link enricher: failed to fetch or extract summary"
                );
            }
        }
    }

    if enrichments.is_empty() {
        return content.to_string();
    }

    let prefix = enrichments.join("\n");
    format!("{prefix}\n{content}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── URL extraction ──────────────────────────────────────────────

    #[test]
    fn extract_urls_finds_http_and_https() {
        let text = "Check https://example.com and http://test.org/page for info";
        let urls = extract_urls(text, 10);
        assert_eq!(urls, vec!["https://example.com", "http://test.org/page",]);
    }

    #[test]
    fn extract_urls_respects_max() {
        let text = "https://a.com https://b.com https://c.com https://d.com";
        let urls = extract_urls(text, 2);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://a.com");
        assert_eq!(urls[1], "https://b.com");
    }

    #[test]
    fn extract_urls_deduplicates() {
        let text = "Visit https://example.com and https://example.com again";
        let urls = extract_urls(text, 10);
        assert_eq!(urls.len(), 1);
    }

    #[test]
    fn extract_urls_handles_no_urls() {
        let text = "Just a normal message without links";
        let urls = extract_urls(text, 10);
        assert!(urls.is_empty());
    }

    #[test]
    fn extract_urls_stops_at_angle_brackets() {
        let text = "Link: <https://example.com/path> done";
        let urls = extract_urls(text, 10);
        assert_eq!(urls, vec!["https://example.com/path"]);
    }

    #[test]
    fn extract_urls_stops_at_quotes() {
        let text = r#"href="https://example.com/page" end"#;
        let urls = extract_urls(text, 10);
        assert_eq!(urls, vec!["https://example.com/page"]);
    }

    // ── SSRF protection ─────────────────────────────────────────────

    #[test]
    fn ssrf_blocks_localhost() {
        assert!(is_ssrf_target("http://localhost/admin"));
        assert!(is_ssrf_target("https://localhost:8080/api"));
        for host in [
            "localhost.",
            "localhost..",
            "foo.localhost...",
            "local.",
            "printer.local..",
            "127.0.0.1..",
        ] {
            assert!(
                is_ssrf_target(&format!("http://{host}/admin")),
                "absolute local hostname {host} must be blocked"
            );
        }
    }

    #[test]
    fn ssrf_blocks_loopback_ip() {
        assert!(is_ssrf_target("http://127.0.0.1/secret"));
        assert!(is_ssrf_target("http://127.0.0.2:9090"));
    }

    #[test]
    fn ssrf_blocks_private_10_network() {
        assert!(is_ssrf_target("http://10.0.0.1/internal"));
        assert!(is_ssrf_target("http://10.255.255.255"));
    }

    #[test]
    fn ssrf_blocks_private_172_network() {
        assert!(is_ssrf_target("http://172.16.0.1/admin"));
        assert!(is_ssrf_target("http://172.31.255.255"));
    }

    #[test]
    fn ssrf_blocks_private_192_168_network() {
        assert!(is_ssrf_target("http://192.168.1.1/router"));
        assert!(is_ssrf_target("http://192.168.0.100:3000"));
    }

    #[test]
    fn ssrf_blocks_link_local() {
        assert!(is_ssrf_target("http://169.254.0.1/metadata"));
        assert!(is_ssrf_target("http://169.254.169.254/latest"));
    }

    #[test]
    fn ssrf_blocks_ipv6_loopback() {
        assert!(is_ssrf_target("http://[::1]/admin"));
        assert!(is_ssrf_target("http://[fd00::1]/admin"));
        assert!(is_ssrf_target("http://[fe80::1]/admin"));
    }

    #[test]
    fn ssrf_blocks_dot_local() {
        assert!(is_ssrf_target("http://myhost.local/api"));
    }

    #[test]
    fn ssrf_allows_public_urls() {
        assert!(!is_ssrf_target("https://example.com/page"));
        assert!(!is_ssrf_target("https://www.google.com"));
        assert!(!is_ssrf_target("http://93.184.216.34/resource"));
        assert!(!is_ssrf_target("https://[2606:4700:4700::1111]/"));
    }

    #[test]
    fn ssrf_rejects_malformed_or_hostless_urls() {
        assert!(is_ssrf_target("not a url"));
        assert!(is_ssrf_target("file:///etc/passwd"));
        assert!(is_ssrf_target("http://"));
        assert!(is_ssrf_target("http://.../"));
    }

    #[tokio::test]
    async fn redirect_policy_rejects_public_to_private_hop() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("http://127.0.0.1:{port}/private")),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/private"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let client = http_client_builder(5)
            .resolve("public.test", *server.address())
            .build()
            .unwrap();
        let url = format!("http://public.test:{port}/start");
        assert!(fetch_link_summary(&client, &url).await.is_none());
    }

    #[tokio::test]
    async fn redirect_policy_rejects_absolute_localhost_hop() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("http://localhost..:{port}/private")),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/private"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let client = http_client_builder(5)
            .resolve("public.test", *server.address())
            .resolve("localhost..", *server.address())
            .build()
            .unwrap();
        let url = format!("http://public.test:{port}/start");
        assert!(fetch_link_summary(&client, &url).await.is_none());
    }

    #[tokio::test]
    async fn redirect_policy_follows_public_hop() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let port = server.address().port();
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/final"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/final"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<title>Redirected</title><p>ok</p>", "text/html"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = http_client_builder(5)
            .resolve("public.test.", *server.address())
            .build()
            .unwrap();
        let summary = fetch_link_summary(&client, &format!("http://public.test.:{port}/start"))
            .await
            .expect("public redirect should succeed");
        assert_eq!(summary.title, "redirected");
    }

    async fn mount_redirect_chain(server: &wiremock::MockServer, redirects: usize) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        for hop in 0..=redirects {
            let response = if hop < redirects {
                ResponseTemplate::new(302).insert_header("Location", format!("/hop/{}", hop + 1))
            } else {
                ResponseTemplate::new(200)
            };
            Mock::given(method("GET"))
                .and(path(format!("/hop/{hop}")))
                .respond_with(response)
                .mount(server)
                .await;
        }
    }

    #[tokio::test]
    async fn redirect_policy_preserves_five_redirect_limit() {
        use wiremock::MockServer;

        let server = MockServer::start().await;
        mount_redirect_chain(&server, MAX_REDIRECTS).await;
        let client = http_client_builder(5)
            .resolve("public.test", *server.address())
            .build()
            .unwrap();
        let response = client
            .get(format!(
                "http://public.test:{}/hop/0",
                server.address().port()
            ))
            .send()
            .await
            .expect("five redirects should remain allowed");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn redirect_policy_rejects_sixth_redirect() {
        use wiremock::MockServer;

        let server = MockServer::start().await;
        mount_redirect_chain(&server, MAX_REDIRECTS + 1).await;
        let client = http_client_builder(5)
            .resolve("public.test", *server.address())
            .build()
            .unwrap();
        let error = client
            .get(format!(
                "http://public.test:{}/hop/0",
                server.address().port()
            ))
            .send()
            .await
            .expect_err("sixth redirect must fail");
        assert!(error.is_redirect());
    }

    // ── Title extraction ────────────────────────────────────────────

    #[test]
    fn extract_title_basic() {
        let html = "<html><head><title>My Page Title</title></head><body>Hello</body></html>";
        assert_eq!(extract_title(html), Some("my page title".to_string()));
    }

    #[test]
    fn extract_title_with_entities() {
        let html = "<title>Tom &amp; Jerry&#39;s Page</title>";
        assert_eq!(extract_title(html), Some("tom & jerry's page".to_string()));
    }

    #[test]
    fn extract_title_case_insensitive() {
        let html = "<HTML><HEAD><TITLE>Upper Case</TITLE></HEAD></HTML>";
        assert_eq!(extract_title(html), Some("upper case".to_string()));
    }

    #[test]
    fn extract_title_multibyte_chars_no_panic() {
        // İ (U+0130) lowercases to 2 chars, changing byte length.
        // This must not panic or produce wrong offsets.
        let html = "<title>İstanbul Guide</title>";
        let result = extract_title(html);
        assert!(result.is_some());
        let title = result.unwrap();
        assert!(title.contains("stanbul"));
    }

    #[test]
    fn extract_title_missing() {
        let html = "<html><body>No title here</body></html>";
        assert_eq!(extract_title(html), None);
    }

    #[test]
    fn extract_title_empty() {
        let html = "<title>   </title>";
        assert_eq!(extract_title(html), None);
    }

    // ── Body text extraction ────────────────────────────────────────

    #[test]
    fn extract_body_text_strips_html() {
        let html = "<html><body><h1>Header</h1><p>Some content here</p></body></html>";
        let text = extract_body_text(html, 200);
        assert!(text.contains("Header"));
        assert!(text.contains("Some content"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn extract_body_text_truncates() {
        let html = "<p>A very long paragraph that should be truncated to fit within the limit.</p>";
        let text = extract_body_text(html, 20);
        assert!(text.len() <= 25); // 20 chars + "..."
        assert!(text.ends_with("..."));
    }

    // ── Config toggle ───────────────────────────────────────────────

    #[tokio::test]
    async fn enrich_message_disabled_returns_original() {
        let config = LinkEnricherConfig {
            enabled: false,
            max_links: 3,
            timeout_secs: 10,
        };
        let msg = "Check https://example.com for details";
        let result = enrich_message(msg, &config).await;
        assert_eq!(result, msg);
    }

    #[tokio::test]
    async fn enrich_message_no_urls_returns_original() {
        let config = LinkEnricherConfig {
            enabled: true,
            max_links: 3,
            timeout_secs: 10,
        };
        let msg = "No links in this message";
        let result = enrich_message(msg, &config).await;
        assert_eq!(result, msg);
    }

    #[tokio::test]
    async fn enrich_message_ssrf_urls_returns_original() {
        let config = LinkEnricherConfig {
            enabled: true,
            max_links: 3,
            timeout_secs: 10,
        };
        let msg = "Try http://127.0.0.1/admin and http://192.168.1.1/router";
        let result = enrich_message(msg, &config).await;
        assert_eq!(result, msg);
    }

    #[test]
    fn default_config_is_disabled() {
        let config = LinkEnricherConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_links, 3);
        assert_eq!(config.timeout_secs, 10);
    }
}
