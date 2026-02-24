use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub enum UrlSchemePolicy {
    HttpsOnly,
    HttpOrHttps,
}

#[derive(Debug, Clone)]
pub struct DomainPolicy<'a> {
    pub allowed_domains: &'a [String],
    pub blocked_domains: &'a [String],
    pub allowed_field_name: &'a str,
    pub blocked_field_name: Option<&'a str>,
    pub empty_allowed_message: &'a str,
    pub scheme_policy: UrlSchemePolicy,
    pub ipv6_error_context: &'a str,
}

pub fn validate_url(raw_url: &str, policy: &DomainPolicy<'_>) -> Result<String> {
    let url = raw_url.trim();

    if url.is_empty() {
        anyhow::bail!("URL cannot be empty");
    }

    if url.chars().any(char::is_whitespace) {
        anyhow::bail!("URL cannot contain whitespace");
    }

    if policy.allowed_domains.is_empty() {
        anyhow::bail!("{}", policy.empty_allowed_message);
    }

    let host = extract_host(url, policy.scheme_policy, policy.ipv6_error_context)?;

    if is_private_or_local_host(&host) {
        anyhow::bail!("Blocked local/private host: {host}");
    }

    if let Some(blocked_field_name) = policy.blocked_field_name {
        if host_matches_allowlist(&host, policy.blocked_domains) {
            anyhow::bail!("Host '{host}' is in {blocked_field_name}");
        }
    }

    if !host_matches_allowlist(&host, policy.allowed_domains) {
        anyhow::bail!("Host '{host}' is not in {}", policy.allowed_field_name);
    }

    Ok(url.to_string())
}

pub fn normalize_allowed_domains(domains: Vec<String>) -> Vec<String> {
    let mut normalized = domains
        .into_iter()
        .filter_map(|d| normalize_domain(&d))
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

pub fn normalize_domain(raw: &str) -> Option<String> {
    let mut d = raw.trim().to_lowercase();
    if d.is_empty() {
        return None;
    }

    if let Some(stripped) = d.strip_prefix("https://") {
        d = stripped.to_string();
    } else if let Some(stripped) = d.strip_prefix("http://") {
        d = stripped.to_string();
    }

    if let Some((host, _)) = d.split_once('/') {
        d = host.to_string();
    }

    d = d.trim_start_matches('.').trim_end_matches('.').to_string();

    if let Some((host, _)) = d.split_once(':') {
        d = host.to_string();
    }

    if d.is_empty() || d.chars().any(char::is_whitespace) {
        return None;
    }

    Some(d)
}

pub fn extract_host(
    url: &str,
    scheme_policy: UrlSchemePolicy,
    ipv6_error_context: &str,
) -> anyhow::Result<String> {
    let rest = match scheme_policy {
        UrlSchemePolicy::HttpsOnly => url
            .strip_prefix("https://")
            .ok_or_else(|| anyhow::anyhow!("Only https:// URLs are allowed"))?,
        UrlSchemePolicy::HttpOrHttps => url
            .strip_prefix("http://")
            .or_else(|| url.strip_prefix("https://"))
            .ok_or_else(|| anyhow::anyhow!("Only http:// and https:// URLs are allowed"))?,
    };

    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid URL"))?;

    if authority.is_empty() {
        anyhow::bail!("URL must include a host");
    }

    if authority.contains('@') {
        anyhow::bail!("URL userinfo is not allowed");
    }

    if authority.starts_with('[') {
        anyhow::bail!("IPv6 hosts are not supported in {ipv6_error_context}");
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

pub fn host_matches_allowlist(host: &str, allowed_domains: &[String]) -> bool {
    allowed_domains.iter().any(|pattern| {
        if pattern == "*" {
            return true;
        }

        if let Some(suffix) = pattern.strip_prefix("*.") {
            return host == suffix || host.ends_with(&format!(".{suffix}"));
        }

        host == pattern || host.ends_with(&format!(".{pattern}"))
    })
}

pub fn is_private_or_local_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    let has_local_tld = bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");

    if bare == "localhost" || bare.ends_with(".localhost") || has_local_tld {
        return true;
    }

    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(v6),
        };
    }

    false
}

fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b))
        || a >= 240
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && b == 51)
        || (a == 203 && b == 0)
        || (a == 198 && (18..=19).contains(&b))
}

fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xfe00) == 0xfc00
        || (segs[0] & 0xffc0) == 0xfe80
        || (segs[0] == 0x2001 && segs[1] == 0x0db8)
        || v6.to_ipv4_mapped().is_some_and(is_non_global_v4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_domain_strips_scheme_and_path() {
        let got = normalize_domain("https://Docs.Example.com/path").unwrap();
        assert_eq!(got, "docs.example.com");
    }

    #[test]
    fn normalize_domain_rejects_whitespace() {
        assert!(normalize_domain("exa mple.com").is_none());
    }

    #[test]
    fn normalize_allowed_domains_deduplicates() {
        let got = normalize_allowed_domains(vec![
            "example.com".into(),
            "EXAMPLE.COM".into(),
            "https://example.com/".into(),
        ]);
        assert_eq!(got, vec!["example.com".to_string()]);
    }

    #[test]
    fn host_matches_allowlist_exact() {
        assert!(host_matches_allowlist(
            "example.com",
            &["example.com".into()]
        ));
    }

    #[test]
    fn host_matches_allowlist_subdomain() {
        assert!(host_matches_allowlist(
            "docs.example.com",
            &["example.com".into()]
        ));
    }

    #[test]
    fn host_matches_allowlist_wildcard_pattern() {
        assert!(host_matches_allowlist(
            "api.example.com",
            &["*.example.com".into()]
        ));
        assert!(host_matches_allowlist(
            "example.com",
            &["*.example.com".into()]
        ));
    }

    #[test]
    fn host_matches_allowlist_global_wildcard() {
        assert!(host_matches_allowlist("example.net", &["*".into()]));
    }

    #[test]
    fn extract_host_supports_http_and_https() {
        let host = extract_host(
            "http://api.example.com/path",
            UrlSchemePolicy::HttpOrHttps,
            "http_request",
        )
        .unwrap();
        assert_eq!(host, "api.example.com");
    }

    #[test]
    fn extract_host_rejects_non_https_when_https_only() {
        let err = extract_host(
            "http://example.com",
            UrlSchemePolicy::HttpsOnly,
            "browser_open",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Only https://"));
    }

    #[test]
    fn extract_host_rejects_userinfo() {
        let err = extract_host(
            "https://user@example.com",
            UrlSchemePolicy::HttpsOnly,
            "browser_open",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("userinfo"));
    }

    #[test]
    fn extract_host_rejects_ipv6_literal() {
        let err = extract_host(
            "https://[::1]:8443",
            UrlSchemePolicy::HttpsOnly,
            "browser_open",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("IPv6"));
    }

    #[test]
    fn private_host_detection_localhost() {
        assert!(is_private_or_local_host("localhost"));
        assert!(is_private_or_local_host("api.localhost"));
    }

    #[test]
    fn private_host_detection_private_ipv4() {
        assert!(is_private_or_local_host("10.0.0.1"));
        assert!(is_private_or_local_host("172.16.0.1"));
        assert!(is_private_or_local_host("192.168.1.5"));
    }

    #[test]
    fn private_host_detection_public_ipv4() {
        assert!(!is_private_or_local_host("8.8.8.8"));
    }

    #[test]
    fn private_host_detection_ipv6_local() {
        assert!(is_private_or_local_host("::1"));
        assert!(is_private_or_local_host("fd00::1"));
    }

    #[test]
    fn private_host_detection_ipv6_public() {
        assert!(!is_private_or_local_host("2607:f8b0:4004:800::200e"));
    }

    fn policy<'a>(
        allowed_domains: &'a [String],
        blocked_domains: &'a [String],
    ) -> DomainPolicy<'a> {
        DomainPolicy {
            allowed_domains,
            blocked_domains,
            allowed_field_name: "web_fetch.allowed_domains",
            blocked_field_name: Some("web_fetch.blocked_domains"),
            empty_allowed_message: "allowed domains must be configured",
            scheme_policy: UrlSchemePolicy::HttpOrHttps,
            ipv6_error_context: "web_fetch",
        }
    }

    #[test]
    fn validate_url_accepts_public_allowed_host() {
        let allowed = vec!["example.com".to_string()];
        let blocked: Vec<String> = Vec::new();
        let got =
            validate_url("https://docs.example.com/path", &policy(&allowed, &blocked)).unwrap();
        assert_eq!(got, "https://docs.example.com/path");
    }

    #[test]
    fn validate_url_rejects_blocked_host() {
        let allowed = vec!["*".to_string()];
        let blocked = vec!["example.com".to_string()];
        let err = validate_url("https://example.com", &policy(&allowed, &blocked))
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn validate_url_rejects_private_host() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let err = validate_url("https://127.0.0.1", &policy(&allowed, &blocked))
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_url_rejects_allowlist_miss() {
        let allowed = vec!["example.com".to_string()];
        let blocked: Vec<String> = Vec::new();
        let err = validate_url("https://google.com", &policy(&allowed, &blocked))
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_url_rejects_empty_allowlist() {
        let allowed: Vec<String> = Vec::new();
        let blocked: Vec<String> = Vec::new();
        let err = validate_url("https://example.com", &policy(&allowed, &blocked))
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed domains must be configured"));
    }
}
