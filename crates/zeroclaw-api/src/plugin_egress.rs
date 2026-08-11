//! Dependency-free value validation shared by plugin config and host egress.

/// A validated exact, subdomain-wildcard, or all-hosts policy pattern.
///
/// Construction is centralized here so config validation and runtime matching
/// cannot drift onto different hostname grammars.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutboundHostPattern(OutboundHostPatternKind);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum OutboundHostPatternKind {
    Any,
    Exact(String),
    Subdomains(String),
}

impl OutboundHostPattern {
    /// Parse `*`, an exact DNS host/IP, or a `*.example.com` subdomain pattern.
    #[must_use]
    pub fn parse(pattern: &str) -> Option<Self> {
        if pattern == "*" {
            return Some(Self(OutboundHostPatternKind::Any));
        }
        if let Some(domain) = pattern.strip_prefix("*.") {
            let domain = normalize_outbound_host(domain)?;
            if domain.parse::<std::net::IpAddr>().is_ok() {
                return None;
            }
            return Some(Self(OutboundHostPatternKind::Subdomains(domain)));
        }
        normalize_outbound_host(pattern)
            .map(OutboundHostPatternKind::Exact)
            .map(Self)
    }

    /// Match an already-normalized host.
    ///
    /// `*.example.com` matches subdomains only, never the zone apex.
    #[must_use]
    pub fn matches_normalized(&self, host: &str) -> bool {
        match &self.0 {
            OutboundHostPatternKind::Any => true,
            OutboundHostPatternKind::Exact(expected) => host == expected,
            OutboundHostPatternKind::Subdomains(domain) => host
                .strip_suffix(domain)
                .is_some_and(|prefix| !prefix.is_empty() && prefix.ends_with('.')),
        }
    }
}

/// Canonicalize one outbound DNS host or IP literal.
///
/// DNS names are ASCII-only; internationalized names must use their punycode
/// form. IPv6 brackets are accepted and removed. Userinfo, ports, paths, empty
/// labels, whitespace, and control characters are rejected.
#[must_use]
pub fn normalize_outbound_host(host: &str) -> Option<String> {
    if host.is_empty() || host.trim() != host || host.chars().any(char::is_control) {
        return None;
    }

    let (host, bracketed) = match (host.starts_with('['), host.ends_with(']')) {
        (true, true) => (host.strip_prefix('[')?.strip_suffix(']')?, true),
        (false, false) => (host, false),
        _ => return None,
    };
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if bracketed && !ip.is_ipv6() {
            return None;
        }
        return Some(ip.to_string().to_ascii_lowercase());
    }
    if bracketed {
        return None;
    }

    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    valid.then_some(host)
}

/// Whether `name` is a portable lowercase TLS-profile slug.
#[must_use]
pub fn is_valid_tls_profile_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_normalization_is_canonical_and_strict() {
        assert_eq!(
            normalize_outbound_host("API.Example.COM."),
            Some("api.example.com".to_string())
        );
        assert_eq!(
            normalize_outbound_host("[2001:4860:4860::8888]"),
            Some("2001:4860:4860::8888".to_string())
        );
        for invalid in ["", " api.example", "bad..example", "[127.0.0.1]"] {
            assert!(normalize_outbound_host(invalid).is_none(), "{invalid:?}");
        }
    }

    #[test]
    fn wildcard_matches_subdomains_but_not_apex_or_suffix_confusion() {
        let pattern = OutboundHostPattern::parse("*.Example.COM.").unwrap();
        assert!(pattern.matches_normalized("api.example.com"));
        assert!(!pattern.matches_normalized("example.com"));
        assert!(!pattern.matches_normalized("notexample.com"));
        assert!(OutboundHostPattern::parse("*.127.0.0.1").is_none());
    }

    #[test]
    fn profile_identifiers_reject_non_slug_syntax() {
        assert!(is_valid_tls_profile_name("corporate-mtls"));
        assert!(!is_valid_tls_profile_name("Corporate"));
    }
}
