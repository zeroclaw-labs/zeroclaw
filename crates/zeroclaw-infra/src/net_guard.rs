//! Network-safety primitives shared across crates that must reject SSRF and
//! local/private targets. Lives in `zeroclaw-infra` so both the tool layer
//! (`zeroclaw-tools` domain guard) and the plugin host (`zeroclaw-plugins`
//! `wasi:http` egress) read one implementation without a tool-to-plugin
//! dependency.
//!
//! Everything here operates on plain data — host strings, IP addresses, and
//! pattern lists — so no consumer needs a tool-specific or config-specific
//! type to ask "may this process reach that destination". DNS resolution is
//! deliberately *not* part of this module: callers resolve, then hand the
//! resolved addresses here for validation.
//!
//! The pieces are:
//!
//! - [`normalize_domain`] / [`normalize_allowed_domains`]: turn operator-authored
//!   allowlist entries into canonical bare hosts.
//! - [`host_matches_allowlist`]: match a request host against those entries.
//! - [`is_cloud_metadata_ip`], [`is_private_or_local_host`], [`is_non_global_v4`],
//!   [`is_non_global_v6`]: address-class classification.
//! - [`validate_resolved_ips_are_public`] /
//!   [`validate_resolved_ips_exclude_metadata`]: post-resolution SSRF checks.

// ── allowlist normalization ───────────────────────────────────────
// Operator-authored entries may be written as bare hosts, bracketed IPv6,
// or full URLs; normalization reduces them all to a canonical lowercase
// bare host so matching never has to re-parse.

/// Normalize a single allowlist entry to a canonical bare host.
///
/// Accepts bare hosts, bare IPv4/IPv6 literals (bracketed or not), and full
/// URLs (a missing scheme is treated as `https://`). Returns `None` for empty
/// input, input containing whitespace, unmatched brackets, entries carrying
/// userinfo, or anything that does not parse to a host.
#[must_use]
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

    let parsed = url::Url::parse(input)
        .or_else(|_| url::Url::parse(&format!("https://{input}")))
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

/// Normalize a whole allowlist, sorted and deduplicated.
///
/// `label` names the configuration surface in the error message so the
/// operator can find the offending entry (for example
/// `"http_request.allowed_domains"`). Fails if any entry is not a valid
/// domain, hostname, IPv4, or IPv6 address.
///
/// # Errors
///
/// Returns an error naming every rejected entry when one or more entries
/// fail [`normalize_domain`].
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

// ── host matching ─────────────────────────────────────────────────

/// True when `host` matches any entry in a normalized `allowed` list.
///
/// Matching rules, unchanged from the tool-layer original:
/// - a bare `*` entry matches everything;
/// - a `*.example.com` entry matches `example.com` and any subdomain;
/// - an IP entry, or an IP host, matches only exactly;
/// - a bare domain entry matches itself and any subdomain of it.
///
/// These are the permissive semantics the tool-layer `allowed_domains` lists
/// have always used. A consumer that needs stricter rules — no bare `*`, no
/// implicit subdomains — must enforce that when it validates its own entries,
/// or use a separate matcher; this function will not reject them.
#[must_use]
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

// ── address-class classification ──────────────────────────────────

/// True when `host` is loopback, private, link-local, a documentation/
/// benchmark range, or one of the `localhost` / `*.local` name forms. Accepts
/// bracketed IPv6 (`[::1]`) and is case-insensitive.
#[must_use]
pub fn is_private_or_local_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();

    if &bare == "localhost" || bare.ends_with(".localhost") {
        return true;
    }

    if bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local")
    {
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

/// True when an IPv4 address is not globally routable (loopback, RFC 1918,
/// link-local, CGNAT, documentation, benchmarking, reserved, multicast).
#[must_use]
pub fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b)) // RFC 6598 shared address space
        || a >= 240 // Reserved
        || (a == 192 && b == 0 && (c == 0 || c == 2)) // 192.0.0.0/24, 192.0.2.0/24
        || (a == 198 && b == 51) // Documentation (198.51.100.0/24)
        || (a == 203 && b == 0) // Documentation (203.0.113.0/24)
        || (a == 198 && (18..=19).contains(&b)) // Benchmarking (198.18.0.0/15)
}

/// True when an IPv6 address is not globally routable (loopback, ULA,
/// link-local, documentation, multicast, or an IPv4-mapped non-global v4).
#[must_use]
pub fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xfe00) == 0xfc00 // Unique-local (fc00::/7)
        || (segs[0] & 0xffc0) == 0xfe80 // Link-local (fe80::/10)
        || (segs[0] == 0x2001 && segs[1] == 0x0db8) // Documentation (2001:db8::/32)
        || v6.to_ipv4_mapped().is_some_and(is_non_global_v4)
}

/// True when `ip` is a cloud instance-metadata service address.
///
/// Metadata addresses are refused unconditionally by both
/// [`validate_resolved_ips_are_public`] and
/// [`validate_resolved_ips_exclude_metadata`], so an operator opt-in for
/// private destinations never re-opens them.
#[must_use]
pub fn is_cloud_metadata_ip(ip: std::net::IpAddr) -> bool {
    const EC2_IMDS_V4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(169, 254, 169, 254);
    const EC2_IMDS_V6: std::net::Ipv6Addr =
        std::net::Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

    match ip {
        std::net::IpAddr::V4(v4) => v4 == EC2_IMDS_V4,
        std::net::IpAddr::V6(v6) => {
            v6 == EC2_IMDS_V6 || v6.to_ipv4_mapped().is_some_and(|v4| v4 == EC2_IMDS_V4)
        }
    }
}

// ── resolved-address validation ───────────────────────────────────
// These helpers only classify the supplied answer. To prevent DNS rebinding,
// callers must connect to the exact addresses they validated rather than
// resolving the hostname again.

/// Reject a resolution that contains any metadata or non-globally-routable
/// address. This is the default post-resolution SSRF check.
///
/// # Errors
///
/// Returns an error when `ips` is empty, contains a cloud metadata address,
/// or contains any non-globally-routable address.
pub fn validate_resolved_ips_are_public(
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

/// Reject a resolution that contains a metadata address, but permit private
/// and loopback addresses. For callers that carry an explicit operator
/// opt-in for private destinations; metadata stays blocked regardless.
///
/// # Errors
///
/// Returns an error when `ips` is empty or contains a cloud metadata address.
pub fn validate_resolved_ips_exclude_metadata(
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
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn blocks_rfc1918_and_loopback_and_metadata() {
        for h in [
            "127.0.0.1",
            "localhost",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "[::1]",
            "fe80::1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private_or_local_host(h), "{h} must be blocked");
        }
    }

    #[test]
    fn allows_public() {
        for h in [
            "1.1.1.1",
            "8.8.8.8",
            "example.com",
            "[2606:4700:4700::1111]",
        ] {
            assert!(!is_private_or_local_host(h), "{h} must be allowed");
        }
    }

    #[test]
    fn ipv4_mapped_v6_follows_v4_classification() {
        assert!(is_non_global_v6(
            "::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap()
        ));
        assert!(!is_non_global_v6(
            "::ffff:1.1.1.1".parse::<Ipv6Addr>().unwrap()
        ));
    }

    #[test]
    fn cloud_metadata_detection_normalizes_ipv4_mapped_ipv6() {
        let mapped = "::ffff:169.254.169.254".parse().unwrap();
        assert!(is_cloud_metadata_ip(mapped));
    }

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
    fn validate_resolved_ips_blocks_mapped_metadata_even_for_private_opt_in() {
        let ips = ["::ffff:169.254.169.254".parse().unwrap()];
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

    #[test]
    fn cgnat_and_reserved_v4_blocked() {
        assert!(is_non_global_v4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_non_global_v4(Ipv4Addr::new(240, 0, 0, 1)));
    }

    #[test]
    fn dot_local_and_dot_localhost_subdomains_are_private() {
        // mDNS .local names must be blocked (RFC 6762)
        assert!(is_private_or_local_host("mydevice.local"));
        assert!(is_private_or_local_host("printer.local"));
        // *.localhost subdomains must be blocked (RFC 2606)
        assert!(is_private_or_local_host("foo.localhost"));
        assert!(is_private_or_local_host("app.localhost"));
        // Public domain that merely ends in "local" as a substring must not match
        assert!(!is_private_or_local_host("notlocal.com"));
    }

    #[test]
    fn rfc5737_documentation_and_rfc2544_benchmarking_ranges_are_private() {
        // RFC 5737 TEST-NET-1/2/3 documentation ranges
        assert!(is_non_global_v4(Ipv4Addr::new(192, 0, 2, 1)));
        assert!(is_non_global_v4(Ipv4Addr::new(198, 51, 100, 1)));
        assert!(is_non_global_v4(Ipv4Addr::new(203, 0, 113, 1)));
        // RFC 2544 benchmarking range (198.18.0.0/15)
        assert!(is_non_global_v4(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(is_non_global_v4(Ipv4Addr::new(198, 19, 255, 255)));
    }
}
