use crate::config::UrlAccessConfig;
use anyhow::{Context, Result};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

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
    pub url_access: Option<&'a UrlAccessConfig>,
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

    if let Some(blocked_field_name) = policy.blocked_field_name {
        if host_matches_allowlist(&host, policy.blocked_domains) {
            anyhow::bail!("Host '{host}' is in {blocked_field_name}");
        }
    }

    if !host_matches_allowlist(&host, policy.allowed_domains) {
        anyhow::bail!("Host '{host}' is not in {}", policy.allowed_field_name);
    }

    enforce_global_domain_access_policy(&host, policy.url_access)?;
    enforce_private_host_policy(&host, policy.url_access)?;

    Ok(url.to_string())
}

fn enforce_global_domain_access_policy(
    host: &str,
    url_access: Option<&UrlAccessConfig>,
) -> Result<()> {
    let config = url_access.cloned().unwrap_or_default();

    if host_matches_allowlist(host, &config.domain_blocklist) {
        anyhow::bail!("Host '{host}' is blocked by security.url_access.domain_blocklist");
    }

    if config.enforce_domain_allowlist {
        if config.domain_allowlist.is_empty() {
            anyhow::bail!(
                "security.url_access.enforce_domain_allowlist=true but security.url_access.domain_allowlist is empty"
            );
        }
        if !host_matches_allowlist(host, &config.domain_allowlist) {
            anyhow::bail!("Host '{host}' is not in security.url_access.domain_allowlist");
        }
    }

    if config.require_first_visit_approval
        && !host_matches_allowlist(host, &config.domain_allowlist)
        && !host_matches_allowlist(host, &config.approved_domains)
    {
        anyhow::bail!(
            "First-time domain approval required for '{host}'. Ask a human to confirm and then add it to security.url_access.approved_domains (for example via web_access_config)."
        );
    }

    Ok(())
}

fn enforce_private_host_policy(host: &str, url_access: Option<&UrlAccessConfig>) -> Result<()> {
    let config = url_access.cloned().unwrap_or_default();
    if !config.block_private_ip {
        return Ok(());
    }

    // Domain allowlist has highest priority for private/local blocking.
    if host_matches_allowlist(host, &config.allow_domains) {
        return Ok(());
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_non_global_ip(ip) && !is_ip_explicitly_allowed(ip, &config) {
            anyhow::bail!("Blocked local/private host: {host}");
        }
        return Ok(());
    }

    if is_local_hostname(host) && !config.allow_loopback {
        anyhow::bail!("Blocked local/private host: {host}");
    }

    // DNS rebinding defense: resolve host and deny if any resolved address is
    // private/local unless explicitly allowlisted.
    let mut resolved = Vec::new();
    for default_port in [80_u16, 443_u16] {
        let lookup = (host, default_port).to_socket_addrs();
        if let Ok(addrs) = lookup {
            resolved.extend(addrs.map(|addr: SocketAddr| addr.ip()));
            if !resolved.is_empty() {
                break;
            }
        }
    }

    for ip in resolved {
        if is_non_global_ip(ip) && !is_ip_explicitly_allowed(ip, &config) {
            anyhow::bail!("Blocked local/private host after DNS resolution: {host} -> {ip}");
        }
    }

    Ok(())
}

fn is_ip_explicitly_allowed(ip: IpAddr, config: &UrlAccessConfig) -> bool {
    if config.allow_loopback && ip.is_loopback() {
        return true;
    }

    config
        .allow_cidrs
        .iter()
        .filter_map(|raw| parse_cidr(raw).ok())
        .any(|cidr| cidr_contains_ip(cidr, ip))
}

fn is_non_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_global_v4(v4),
        IpAddr::V6(v6) => is_non_global_v6(v6),
    }
}

fn parse_cidr(raw: &str) -> anyhow::Result<(IpAddr, u8)> {
    let (ip_raw, prefix_raw) = raw
        .trim()
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("missing '/' separator"))?;
    let ip = ip_raw
        .trim()
        .parse::<IpAddr>()
        .with_context(|| format!("invalid IP '{ip_raw}'"))?;
    let prefix = prefix_raw
        .trim()
        .parse::<u8>()
        .with_context(|| format!("invalid prefix '{prefix_raw}'"))?;
    let max_prefix = match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    if prefix > max_prefix {
        anyhow::bail!("prefix {prefix} exceeds max {max_prefix}");
    }
    Ok((ip, prefix))
}

fn cidr_contains_ip(cidr: (IpAddr, u8), ip: IpAddr) -> bool {
    match (cidr.0, ip) {
        (IpAddr::V4(net), IpAddr::V4(candidate)) => {
            let net_u32 = u32::from(net);
            let ip_u32 = u32::from(candidate);
            let prefix = cidr.1;
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (net_u32 & mask) == (ip_u32 & mask)
        }
        (IpAddr::V6(net), IpAddr::V6(candidate)) => {
            let net_u128 = u128::from(net);
            let ip_u128 = u128::from(candidate);
            let prefix = cidr.1;
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (net_u128 & mask) == (ip_u128 & mask)
        }
        _ => false,
    }
}

fn is_local_hostname(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let has_local_tld = bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");
    bare == "localhost" || bare.ends_with(".localhost") || has_local_tld
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

    if is_local_hostname(bare) {
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
            url_access: None,
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

    #[test]
    fn validate_url_allows_private_ip_when_cidr_allowlisted() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let url_access = UrlAccessConfig {
            allow_cidrs: vec!["10.0.0.0/8".to_string()],
            ..UrlAccessConfig::default()
        };
        let policy = DomainPolicy {
            url_access: Some(&url_access),
            ..policy(&allowed, &blocked)
        };
        let got = validate_url("https://10.1.2.3", &policy).unwrap();
        assert_eq!(got, "https://10.1.2.3");
    }

    #[test]
    fn validate_url_allows_localhost_when_domain_allowlisted() {
        let allowed = vec!["localhost".to_string()];
        let blocked: Vec<String> = Vec::new();
        let url_access = UrlAccessConfig {
            allow_domains: vec!["localhost".to_string()],
            ..UrlAccessConfig::default()
        };
        let policy = DomainPolicy {
            url_access: Some(&url_access),
            ..policy(&allowed, &blocked)
        };
        let got = validate_url("https://localhost:8080", &policy).unwrap();
        assert_eq!(got, "https://localhost:8080");
    }

    #[test]
    fn validate_url_rejects_localhost_when_not_allowlisted() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let err = validate_url("https://localhost:8080", &policy(&allowed, &blocked))
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_url_rejects_domain_blocklist_match() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let url_access = UrlAccessConfig {
            domain_blocklist: vec!["example.com".to_string()],
            ..UrlAccessConfig::default()
        };
        let policy = DomainPolicy {
            url_access: Some(&url_access),
            ..policy(&allowed, &blocked)
        };
        let err = validate_url("https://docs.example.com", &policy)
            .unwrap_err()
            .to_string();
        assert!(err.contains("domain_blocklist"));
    }

    #[test]
    fn validate_url_enforce_global_allowlist_rejects_miss() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let url_access = UrlAccessConfig {
            enforce_domain_allowlist: true,
            domain_allowlist: vec!["rust-lang.org".to_string()],
            ..UrlAccessConfig::default()
        };
        let policy = DomainPolicy {
            url_access: Some(&url_access),
            ..policy(&allowed, &blocked)
        };
        let err = validate_url("https://docs.rs", &policy)
            .unwrap_err()
            .to_string();
        assert!(err.contains("security.url_access.domain_allowlist"));
    }

    #[test]
    fn validate_url_requires_first_visit_approval_for_unseen_domain() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let url_access = UrlAccessConfig {
            require_first_visit_approval: true,
            ..UrlAccessConfig::default()
        };
        let policy = DomainPolicy {
            url_access: Some(&url_access),
            ..policy(&allowed, &blocked)
        };
        let err = validate_url("https://docs.rs", &policy)
            .unwrap_err()
            .to_string();
        assert!(err.contains("First-time domain approval required"));
    }

    #[test]
    fn validate_url_allows_first_visit_when_domain_is_preapproved() {
        let allowed = vec!["*".to_string()];
        let blocked: Vec<String> = Vec::new();
        let url_access = UrlAccessConfig {
            require_first_visit_approval: true,
            approved_domains: vec!["docs.rs".to_string()],
            ..UrlAccessConfig::default()
        };
        let policy = DomainPolicy {
            url_access: Some(&url_access),
            ..policy(&allowed, &blocked)
        };
        let got = validate_url("https://docs.rs", &policy).unwrap();
        assert_eq!(got, "https://docs.rs");
    }
}
