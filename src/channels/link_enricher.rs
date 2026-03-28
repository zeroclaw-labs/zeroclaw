//! Link enricher: auto-detects URLs in inbound messages, fetches their content,
//! and prepends summaries so the agent has link context without explicit tool calls.

use regex::Regex;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

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

/// URL regex: matches http:// and https:// URLs, stopping at whitespace, angle
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
    let host = match extract_host(url) {
        Some(h) => h,
        None => return true, // unparseable URLs are rejected
    };

    // Check hostname-based locals
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "local"
    {
        return true;
    }

    // Check IP-based private ranges
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_private_ip(ip);
    }

    false
}

/// Extract the host portion from a URL string.
fn extract_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // Strip port
    let host = if authority.starts_with('[') {
        // IPv6 in brackets — reject for simplicity
        return None;
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    Some(host.to_lowercase())
}

/// Check if an IP address falls within private/reserved ranges.
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()           // 127.0.0.0/8
                || v4.is_private()     // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()  // 169.254.0.0/16
                || v4.is_unspecified() // 0.0.0.0
                || v4.is_broadcast()   // 255.255.255.255
                || v4.is_multicast() // 224.0.0.0/4
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()       // ::1
                || v6.is_unspecified() // ::
                || v6.is_multicast()
                // Check for IPv4-mapped IPv6 addresses
                || v6.to_ipv4_mapped().is_some_and(|v4| {
                    v4.is_loopback()
                        || v4.is_private()
                        || v4.is_link_local()
                        || v4.is_unspecified()
                })
        }
    }
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
async fn fetch_link_summary(url: &str, timeout_secs: u64) -> Option<LinkSummary> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("ZeroClaw/0.1 (link-enricher)")
        .build()
        .ok()?;

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

/// Enrich a message by prepending link summaries for any URLs found in the text.
///
/// This is the main entry point called from the channel message processing pipeline.
/// If the enricher is disabled or no URLs are found, the original message is returned
/// unchanged.
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

    let mut enrichments = Vec::new();
    for url in safe_urls {
        match fetch_link_summary(url, config.timeout_secs).await {
            Some(summary) => {
                enrichments.push(format!("[Link: {} — {}]", summary.title, summary.snippet));
            }
            None => {
                tracing::debug!(url, "Link enricher: failed to fetch or extract summary");
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
        // IPv6 in brackets is rejected by extract_host
        assert!(is_ssrf_target("http://[::1]/admin"));
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
