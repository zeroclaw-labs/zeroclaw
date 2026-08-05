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

// ── strict egress grammar and matching ────────────────────────────
// The tool-layer `allowed_domains` semantics above are deliberately
// permissive: a bare `*` matches everything and a bare domain implies its
// subdomains. Plugin egress policy (ADR-013) requires the opposite defaults,
// so it gets its own grammar and its own matcher rather than a flag on the
// permissive one. Keeping them as separate functions means a caller cannot
// accidentally inherit the loose rules by forgetting an argument.

/// Canonicalize one egress allowlist entry, or explain why it is rejected.
///
/// This is the single grammar for both halves of ADR-013's split: the signed
/// manifest's `egress.hosts` declaration and the operator's
/// `plugins.entries[].egress_hosts` grant. Both validate here so a pattern that
/// a publisher can declare is exactly a pattern an operator can grant.
///
/// Accepted forms:
/// - an **exact host**: a lowercase domain name, IPv4 literal, or IPv6 literal
///   (bare `::1` or bracketed `[::1]`; both canonicalize to bare). It matches
///   that host and nothing else — a bare domain never implies its subdomains.
/// - an **explicit suffix pattern** `*.example.com`: matches strict subdomains
///   of `example.com` and **not** the apex itself. Grant the apex by listing it
///   separately.
///
/// Rejected, each with a message naming the reason:
/// - a bare `*`. There is no "everything" pattern; deny-by-default has no
///   escape hatch at the grammar level.
/// - empty or whitespace-bearing entries.
/// - anything carrying a scheme, path, query, fragment, or userinfo (`/`, `@`,
///   `?`, `#`, `\`), so an entry is never a URL that silently loses its path.
/// - a `*` anywhere other than the leading `*.` (no `a*.b`, no `*.*.c`).
/// - **a port** (`example.com:8443`). Ports are deliberately not part of this
///   slice's grammar: a granted host is granted on *every* port, all-or-nothing.
///   Rejecting the syntax outright keeps an operator from writing a port and
///   believing it narrowed the grant when it would in fact have been dropped.
/// - a wildcard over an IP literal (`*.10.0.0.1`), which has no meaning.
/// - a single-label wildcard suffix (`*.com`, `*.internal`). This is a footgun
///   guard, not a public-suffix check: no public-suffix list is imported here,
///   so `*.co.uk` is **accepted** even though it is just as broad. The rule only
///   removes the most obvious way to write an accidental internet-wide grant.
/// - any entry that is not already canonical (a trailing dot, uppercase, or a
///   form [`normalize_domain`] would silently rewrite), so the file an operator
///   audits reads exactly as the policy that is enforced.
///
/// # Errors
///
/// Returns the human-readable reason the entry is not a legal egress pattern.
pub fn normalize_egress_pattern(raw: &str) -> Result<String, String> {
    let input = raw.trim();
    if input.is_empty() {
        return Err("entry must not be empty".to_string());
    }
    if input.chars().any(char::is_whitespace) {
        return Err(format!("entry {input:?} must not contain whitespace"));
    }
    if let Some(bad) = input
        .chars()
        .find(|c| matches!(c, '/' | '@' | '?' | '#' | '\\'))
    {
        return Err(format!(
            "entry {input:?} must be a bare host or '*.suffix' pattern, not a URL (found {bad:?})"
        ));
    }

    let lower = input.to_ascii_lowercase();
    if lower != input {
        return Err(format!(
            "entry {input:?} must be lowercase (write {lower:?})"
        ));
    }
    if lower == "*" {
        return Err(
            "a bare '*' is not a valid egress entry: there is no allow-all pattern; list exact hosts or '*.suffix' patterns"
                .to_string(),
        );
    }

    let (wildcard, host) = match lower.strip_prefix("*.") {
        Some(rest) => (true, rest),
        None => (false, lower.as_str()),
    };
    if host.contains('*') {
        return Err(format!(
            "entry {input:?} may only use '*' as a leading '*.' suffix pattern"
        ));
    }
    if host.is_empty() {
        return Err(format!("entry {input:?} has an empty suffix after '*.'"));
    }

    // Port detection has to look past IPv6 colons: strip a matched bracket pair
    // first, then treat a remaining ':' in a non-IPv6 host as a port.
    let bare = match (host.starts_with('['), host.ends_with(']')) {
        (true, true) => &host[1..host.len() - 1],
        _ => host,
    };
    if bare.contains(':') && bare.parse::<std::net::Ipv6Addr>().is_err() {
        return Err(format!(
            "entry {input:?} must not carry a port: an egress grant covers every port on the host, so write just the host"
        ));
    }

    let canonical = normalize_domain(host)
        .ok_or_else(|| format!("entry {input:?} is not a valid host or IP literal"))?;
    if canonical != host && format!("[{canonical}]") != host {
        return Err(format!(
            "entry {input:?} is not in canonical form (write {canonical:?})"
        ));
    }

    if wildcard {
        if canonical.parse::<std::net::IpAddr>().is_ok() {
            return Err(format!(
                "entry {input:?} cannot wildcard an IP literal; list the address exactly"
            ));
        }
        if !canonical.contains('.') {
            return Err(format!(
                "entry {input:?} wildcards a single-label suffix, which is far broader than it looks; write a suffix with at least two labels (for example '*.example.com')"
            ));
        }
        return Ok(format!("*.{canonical}"));
    }

    Ok(canonical)
}

/// Validate and canonicalize a whole egress allowlist, sorted and deduplicated.
///
/// `label` names the surface in the error so an operator can find the offending
/// entry (for example `plugins.entries[github].egress_hosts`).
///
/// # Errors
///
/// Returns an error naming every rejected entry and why.
pub fn normalize_egress_patterns(patterns: &[String], label: &str) -> anyhow::Result<Vec<String>> {
    let mut rejected = Vec::new();
    let mut out = Vec::with_capacity(patterns.len());
    for raw in patterns {
        match normalize_egress_pattern(raw) {
            Ok(pattern) => out.push(pattern),
            Err(reason) => rejected.push(reason),
        }
    }
    if !rejected.is_empty() {
        anyhow::bail!("Invalid {label}: {}", rejected.join("; "));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Strict egress matching: the deny-by-default sibling of
/// [`host_matches_allowlist`].
///
/// `allowed` is expected to hold entries already canonicalized by
/// [`normalize_egress_pattern`]. Matching is:
/// - exact host equality, or
/// - `*.example.com` matching a **strict subdomain** of `example.com`.
///
/// An apex host never matches through a bare-domain entry's subdomains, a
/// suffix pattern never matches the apex, and there is no wildcard that matches
/// everything: an entry of `*` (which validation refuses) would only ever match
/// a literal host named `*`, so even an unvalidated list fails closed.
///
/// An **IP-literal host is matched only by an exactly equal entry**, never by a
/// suffix pattern. Addresses have no subdomain structure, so treating one as a
/// dotted name would let a pattern like `*.0.0.1` match `127.0.0.1` — a real
/// hole, since that pattern passes validation as an ordinary two-label suffix.
///
/// An empty `allowed` list matches nothing, which is the ADR-013 default.
#[must_use]
pub fn egress_host_matches(host: &str, allowed: &[String]) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let host_is_ip = host.parse::<std::net::IpAddr>().is_ok();

    allowed
        .iter()
        .any(|pattern| match pattern.strip_prefix('*') {
            // "*.example.com" -> ".example.com"; `ends_with` on the dotted suffix
            // is exactly "strict subdomain of", since the apex lacks the leading
            // dot and a host equal to the suffix itself is not a legal host.
            Some(suffix) if suffix.starts_with('.') => !host_is_ip && host.ends_with(suffix),
            _ => host == *pattern,
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
        std::net::IpAddr::V6(v6) => v6 == EC2_IMDS_V6,
    }
}

// ── resolved-address validation ───────────────────────────────────
// Callers resolve the destination themselves and pass the answer here, so a
// DNS answer cannot change classes between check and connect.

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
    fn cgnat_and_reserved_v4_blocked() {
        assert!(is_non_global_v4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_non_global_v4(Ipv4Addr::new(240, 0, 0, 1)));
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

    // ── strict egress grammar ─────────────────────────────────────

    #[test]
    fn egress_apex_entry_does_not_match_its_subdomains() {
        // The property that separates egress from the permissive tool
        // semantics: `host_matches_allowlist` says yes here, `egress_host_matches`
        // must say no.
        let allowed = vec!["example.com".to_string()];
        assert!(egress_host_matches("example.com", &allowed));
        assert!(
            !egress_host_matches("api.example.com", &allowed),
            "a bare domain must not imply its subdomains"
        );
        assert!(!egress_host_matches("v2.api.example.com", &allowed));
        assert!(
            host_matches_allowlist("api.example.com", &allowed),
            "the permissive tool matcher is unchanged and still allows this"
        );
    }

    #[test]
    fn egress_suffix_pattern_does_not_match_the_apex() {
        let allowed = vec!["*.example.com".to_string()];
        assert!(egress_host_matches("api.example.com", &allowed));
        assert!(egress_host_matches("v2.api.example.com", &allowed));
        assert!(
            !egress_host_matches("example.com", &allowed),
            "a suffix pattern must not implicitly cover the apex"
        );
        assert!(!egress_host_matches("notexample.com", &allowed));
        assert!(!egress_host_matches("evil-example.com", &allowed));
    }

    #[test]
    fn egress_has_no_allow_all_pattern() {
        let err = normalize_egress_pattern("*").unwrap_err();
        assert!(err.contains("allow-all"), "unexpected error: {err}");
        // Even if an unvalidated `*` reached the matcher it would match nothing.
        let smuggled = vec!["*".to_string()];
        assert!(!egress_host_matches("anything.example.com", &smuggled));
        assert!(!egress_host_matches("10.0.0.1", &smuggled));
    }

    #[test]
    fn egress_empty_allowlist_matches_nothing() {
        assert!(!egress_host_matches("example.com", &[]));
        assert!(!egress_host_matches("127.0.0.1", &[]));
    }

    #[test]
    fn egress_pattern_accepts_canonical_hosts_and_suffixes() {
        for entry in [
            "example.com",
            "api.example.com",
            "*.example.com",
            "*.api.example.com",
            "127.0.0.1",
            "10.0.0.1",
            "::1",
            "*.co.uk", // no public-suffix list is imported; documented as accepted
        ] {
            assert_eq!(
                normalize_egress_pattern(entry).as_deref(),
                Ok(entry),
                "{entry} must round-trip unchanged"
            );
        }
        // Bracketed IPv6 canonicalizes to the bare form.
        assert_eq!(normalize_egress_pattern("[::1]").unwrap(), "::1");
    }

    #[test]
    fn egress_pattern_rejects_ports() {
        let err = normalize_egress_pattern("example.com:8443").unwrap_err();
        assert!(err.contains("port"), "unexpected error: {err}");
        assert!(normalize_egress_pattern("[::1]:8443").is_err());
        assert!(normalize_egress_pattern("*.example.com:443").is_err());
    }

    #[test]
    fn egress_pattern_rejects_urls_and_userinfo() {
        for entry in [
            "https://example.com",
            "example.com/path",
            "user@example.com",
            "example.com?q=1",
            "example.com#frag",
        ] {
            assert!(
                normalize_egress_pattern(entry).is_err(),
                "{entry} must be rejected"
            );
        }
    }

    #[test]
    fn egress_pattern_rejects_empty_whitespace_and_noncanonical() {
        assert!(normalize_egress_pattern("").is_err());
        assert!(normalize_egress_pattern("   ").is_err());
        assert!(normalize_egress_pattern("exa mple.com").is_err());
        assert!(
            normalize_egress_pattern("EXAMPLE.com").is_err(),
            "uppercase must be rejected, not silently lowercased"
        );
        assert!(
            normalize_egress_pattern("example.com.").is_err(),
            "a trailing dot must be rejected, not silently stripped"
        );
    }

    #[test]
    fn egress_pattern_rejects_misplaced_wildcards() {
        for entry in ["*.*.example.com", "api*.example.com", "*example.com", "*."] {
            assert!(
                normalize_egress_pattern(entry).is_err(),
                "{entry} must be rejected"
            );
        }
    }

    #[test]
    fn egress_pattern_rejects_single_label_and_ip_wildcards() {
        let err = normalize_egress_pattern("*.com").unwrap_err();
        assert!(err.contains("single-label"), "unexpected error: {err}");
        assert!(normalize_egress_pattern("*.internal").is_err());
        let err = normalize_egress_pattern("*.10.0.0.1").unwrap_err();
        assert!(err.contains("wildcard an IP"), "unexpected error: {err}");
    }

    #[test]
    fn egress_matching_is_case_insensitive_on_the_request_host() {
        let allowed = vec!["example.com".to_string(), "*.example.org".to_string()];
        assert!(egress_host_matches("EXAMPLE.com", &allowed));
        assert!(egress_host_matches("API.Example.ORG", &allowed));
    }

    /// A suffix pattern must never match an IP-literal host.
    ///
    /// Without the IP check, `ends_with(".0.0.1")` makes `*.0.0.1` match
    /// `127.0.0.1` — an innocuous-looking entry that is actually a loopback
    /// grant. [`normalize_egress_pattern`] happens to reject that particular
    /// spelling (the URL parser canonicalizes `0.0.1` to `0.0.0.1`, so it is
    /// not in canonical form), but the matcher must not *depend* on validation
    /// having run: it is called with lists from the manifest, from config, and
    /// from future transports, and it is the last line.
    #[test]
    fn egress_suffix_pattern_never_matches_an_ip_literal() {
        for pattern in ["*.0.0.1", "*.1.1", "*.254", "*.169.254"] {
            assert!(
                !egress_host_matches("127.0.0.1", &[pattern.to_string()]),
                "{pattern} must not match the IP literal 127.0.0.1"
            );
            assert!(
                !egress_host_matches("169.254.169.254", &[pattern.to_string()]),
                "{pattern} must not match the IP literal 169.254.169.254"
            );
        }
        // An exact IP entry still matches.
        assert!(egress_host_matches("127.0.0.1", &["127.0.0.1".to_string()]));
        // And a suffix pattern still does its real job on names.
        assert!(egress_host_matches("host.0.0.1", &["*.0.0.1".to_string()]));
    }

    #[test]
    fn egress_matching_accepts_bracketed_ipv6_request_hosts() {
        let allowed = vec!["::1".to_string()];
        assert!(egress_host_matches("[::1]", &allowed));
        assert!(egress_host_matches("::1", &allowed));
        assert!(!egress_host_matches("[::2]", &allowed));
    }

    #[test]
    fn normalize_egress_patterns_sorts_dedups_and_names_every_rejection() {
        let ok = normalize_egress_patterns(
            &[
                "b.example.com".to_string(),
                "a.example.com".to_string(),
                "a.example.com".to_string(),
            ],
            "test.egress_hosts",
        )
        .unwrap();
        assert_eq!(ok, vec!["a.example.com", "b.example.com"]);

        let err = normalize_egress_patterns(
            &[
                "*".to_string(),
                "ok.example.com".to_string(),
                "".to_string(),
            ],
            "test.egress_hosts",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("Invalid test.egress_hosts"), "{err}");
        assert!(err.contains("allow-all"), "{err}");
        assert!(err.contains("empty"), "{err}");
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
