//! Shared domain/URL validation and allowlist helpers.

pub fn normalize_domain(raw: &str) -> Option<String> {
    let input = raw.trim();
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return None;
    }

    let bare_ip = match (input.starts_with('['), input.ends_with(']')) {
        (true, true) => &input[1..input.len() - 1],
        (false, false) => input,
        _ => return None,
    };
    if let Ok(ip) = bare_ip.parse::<std::net::IpAddr>() {
        return Some(ip.to_string().to_lowercase());
    }

    let parsed = reqwest::Url::parse(input)
        .or_else(|_| reqwest::Url::parse(&format!("https://{input}")))
        .ok()?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }

    let host = parsed.host_str()?;
    let trimmed = host.trim();
    let host_no_brackets = match (trimmed.starts_with('['), trimmed.ends_with(']')) {
        (true, true) => &trimmed[1..trimmed.len() - 1],
        (false, false) => trimmed,
        _ => return None,
    };
    let normalized = host_no_brackets
        .trim_start_matches('.')
        .trim_end_matches('.');
    if normalized.is_empty() {
        return None;
    }

    Some(normalized.to_lowercase())
}

pub fn normalize_allowed_domains(domains: Vec<String>, label: &str) -> anyhow::Result<Vec<String>> {
    let mut rejected = Vec::new();
    let mut normalized = domains
        .into_iter()
        .filter_map(|d| {
            normalize_domain(&d).or_else(|| {
                rejected.push(d.clone());
                None
            })
        })
        .collect::<Vec<_>>();
    if !rejected.is_empty() {
        anyhow::bail!(
            "Invalid {label} entry(s): [{}]. Each entry must be a valid domain, hostname, IPv4, or IPv6 address.",
            rejected.join(", ")
        );
    }
    normalized.sort_unstable();
    normalized.dedup();
    Ok(normalized)
}

pub fn host_matches_allowlist(host: &str, allowed: &[String]) -> bool {
    if allowed.iter().any(|d| d == "*") {
        return true;
    }

    let host_is_ip = host.parse::<std::net::IpAddr>().is_ok();

    allowed.iter().any(|pattern| {
        if pattern.starts_with("*.") {
            let suffix = &pattern[1..]; // ".example.com"
            return host.ends_with(suffix) || host == &pattern[2..];
        }

        if host_is_ip || pattern.parse::<std::net::IpAddr>().is_ok() {
            return host == pattern;
        }

        host == pattern
            || host
                .strip_suffix(pattern)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// Check whether `host` is a private, loopback, link-local, or otherwise
/// non-globally-routable address (SSRF guard).
/// Handles both IPv4 and IPv6, as well as `localhost` and `.local` domains.
pub use zeroclaw_infra::net_guard::is_private_or_local_host;

// ── private IP classification helpers ─────────────────────────────
// Re-exported from the shared infra primitive so the tool layer and the
// plugin host share one implementation (see zeroclaw-infra::net_guard).
pub(crate) use zeroclaw_infra::net_guard::{is_non_global_v4, is_non_global_v6};

pub(crate) fn is_cloud_metadata_ip(ip: std::net::IpAddr) -> bool {
    const EC2_IMDS_V4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(169, 254, 169, 254);
    const EC2_IMDS_V6: std::net::Ipv6Addr =
        std::net::Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

    match ip {
        std::net::IpAddr::V4(v4) => v4 == EC2_IMDS_V4,
        std::net::IpAddr::V6(v6) => v6 == EC2_IMDS_V6,
    }
}

pub(crate) fn validate_resolved_ips_are_public(
    host: &str,
    ips: &[std::net::IpAddr],
) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        if is_cloud_metadata_ip(*ip) {
            anyhow::bail!("Blocked host '{host}' resolved to cloud metadata address {ip}");
        }

        let non_global = match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(*v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(*v6),
        };
        if non_global {
            anyhow::bail!("Blocked host '{host}' resolved to non-global address {ip}");
        }
    }

    Ok(())
}

pub(crate) fn validate_resolved_ips_exclude_metadata(
    host: &str,
    ips: &[std::net::IpAddr],
) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        if is_cloud_metadata_ip(*ip) {
            anyhow::bail!("Blocked host '{host}' resolved to cloud metadata address {ip}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_domain_strips_scheme_path_and_case() {
        let got = normalize_domain("  HTTPS://Docs.Example.com/path ").unwrap();
        assert_eq!(got, "docs.example.com");
    }

    #[test]
    fn normalize_domain_accepts_ipv4() {
        assert_eq!(normalize_domain("192.168.1.1").unwrap(), "192.168.1.1");
        assert_eq!(normalize_domain("127.0.0.1").unwrap(), "127.0.0.1");
    }

    #[test]
    fn normalize_domain_accepts_ipv6() {
        assert_eq!(normalize_domain("[2001:db8::1]").unwrap(), "2001:db8::1");
        assert_eq!(normalize_domain("::1").unwrap(), "::1");
        assert_eq!(normalize_domain("[::1]").unwrap(), "::1");
    }

    #[test]
    fn normalize_domain_rejects_unmatched_brackets() {
        assert!(normalize_domain("[::1").is_none());
        assert!(normalize_domain("::1]").is_none());
        assert!(normalize_domain("[127.0.0.1").is_none());
        assert!(normalize_domain("127.0.0.1]").is_none());
    }

    #[test]
    fn normalize_domain_rejects_userinfo() {
        assert!(normalize_domain("https://user@example.com").is_none());
        assert!(normalize_domain("user@example.com").is_none());
        assert!(normalize_domain("https://user:pass@example.com").is_none());
        assert!(normalize_domain("user:pass@example.com").is_none());
    }

    #[test]
    fn normalize_allowed_domains_deduplicates() {
        let got = normalize_allowed_domains(
            vec![
                "example.com".into(),
                "EXAMPLE.COM".into(),
                "https://example.com/".into(),
            ],
            "test",
        )
        .unwrap();
        assert_eq!(got, vec!["example.com".to_string()]);
    }

    #[test]
    fn normalize_allowed_domains_rejects_invalid() {
        let err = normalize_allowed_domains(
            vec!["example.com".into(), "".into(), "   ".into()],
            "test.config",
        )
        .unwrap_err();
        assert!(err.to_string().contains("Invalid test.config entry"));
    }

    #[test]
    fn host_matches_allowlist_exact() {
        let allowed = vec!["example.com".into()];
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(!host_matches_allowlist("other.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_subdomain() {
        let allowed = vec!["example.com".into()];
        assert!(host_matches_allowlist("api.example.com", &allowed));
        assert!(host_matches_allowlist("v2.api.example.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_wildcard_star() {
        let allowed = vec!["*".into()];
        assert!(host_matches_allowlist("anything.goes.com", &allowed));
        assert!(host_matches_allowlist("192.168.1.1", &allowed));
    }

    #[test]
    fn host_matches_allowlist_wildcard_subdomain() {
        let allowed = vec!["*.example.com".into()];
        assert!(host_matches_allowlist("api.example.com", &allowed));
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(!host_matches_allowlist("other.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_ip_exact_only() {
        let allowed = vec!["10.0.0.1".into(), "2001:db8::1".into()];
        assert!(host_matches_allowlist("10.0.0.1", &allowed));
        assert!(!host_matches_allowlist("10.0.0.2", &allowed));
        assert!(host_matches_allowlist("2001:db8::1", &allowed));
        assert!(!host_matches_allowlist("2001:db8::2", &allowed));
    }

    #[test]
    fn is_private_or_local_host_detects_common() {
        assert!(is_private_or_local_host("localhost"));
        assert!(is_private_or_local_host("sub.localhost"));
        assert!(is_private_or_local_host("myhost.local"));
        assert!(is_private_or_local_host("127.0.0.1"));
        assert!(is_private_or_local_host("10.0.0.1"));
        assert!(is_private_or_local_host("192.168.1.1"));
        assert!(is_private_or_local_host("172.16.0.1"));
        assert!(is_private_or_local_host("::1"));
        assert!(is_private_or_local_host("[::1]"));
        assert!(is_private_or_local_host("fe80::1"));
        assert!(is_private_or_local_host("fc00::1"));
    }

    #[test]
    fn is_private_or_local_host_allows_public() {
        assert!(!is_private_or_local_host("example.com"));
        assert!(!is_private_or_local_host("8.8.8.8"));
        assert!(!is_private_or_local_host("2001:4860:4860::8888"));
    }

    #[test]
    fn is_private_or_local_host_case_insensitive() {
        assert!(is_private_or_local_host("LOCALHOST"));
        assert!(is_private_or_local_host("Sub.LocalHost"));
        assert!(is_private_or_local_host("Printer.LOCAL"));
    }

    #[test]
    fn blocks_multicast_ipv4() {
        assert!(is_private_or_local_host("224.0.0.1"));
        assert!(is_private_or_local_host("239.255.255.255"));
    }

    #[test]
    fn blocks_broadcast() {
        assert!(is_private_or_local_host("255.255.255.255"));
    }

    #[test]
    fn blocks_unspecified() {
        assert!(is_private_or_local_host("0.0.0.0"));
        assert!(is_private_or_local_host("::"));
    }

    #[test]
    fn blocks_reserved_ipv4() {
        assert!(is_private_or_local_host("240.0.0.1"));
        assert!(is_private_or_local_host("250.1.2.3"));
    }

    #[test]
    fn blocks_documentation_ranges() {
        assert!(is_private_or_local_host("192.0.2.1")); // TEST-NET-1
        assert!(is_private_or_local_host("198.51.100.1")); // TEST-NET-2
        assert!(is_private_or_local_host("203.0.113.1")); // TEST-NET-3
    }

    #[test]
    fn blocks_benchmarking_range() {
        assert!(is_private_or_local_host("198.18.0.1"));
        assert!(is_private_or_local_host("198.19.255.255"));
    }

    #[test]
    fn blocks_rfc6598_shared_address_space() {
        assert!(is_private_or_local_host("100.64.0.1"));
        assert!(is_private_or_local_host("100.127.255.255"));
    }

    #[test]
    fn blocks_ipv6_multicast() {
        assert!(is_private_or_local_host("ff02::1"));
    }

    #[test]
    fn blocks_ipv6_unique_local_fd00() {
        assert!(is_private_or_local_host("fd00::1"));
    }

    #[test]
    fn blocks_ipv4_mapped_ipv6() {
        assert!(is_private_or_local_host("::ffff:127.0.0.1"));
        assert!(is_private_or_local_host("::ffff:192.168.1.1"));
        assert!(is_private_or_local_host("::ffff:10.0.0.1"));
    }

    #[test]
    fn blocks_ipv6_documentation_range() {
        assert!(is_private_or_local_host("2001:db8::1"));
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(!is_private_or_local_host("8.8.8.8"));
        assert!(!is_private_or_local_host("1.1.1.1"));
        assert!(!is_private_or_local_host("93.184.216.34"));
    }

    #[test]
    fn allows_public_ipv6() {
        assert!(!is_private_or_local_host("2607:f8b0:4004:800::200e"));
    }

    #[test]
    fn validate_resolved_ips_blocks_private_resolution() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1))];
        let err = validate_resolved_ips_are_public("example.com", &ips)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-global address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_blocks_metadata_even_for_private_opt_in() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            169, 254, 169, 254,
        ))];
        let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_blocks_ec2_ipv6_metadata_even_for_private_opt_in() {
        let ips = ["fd00:ec2::254".parse().unwrap()];
        let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_metadata_is_not_reported_as_generic_private() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            169, 254, 169, 254,
        ))];
        let err = validate_resolved_ips_are_public("metadata.test", &ips)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }
}
