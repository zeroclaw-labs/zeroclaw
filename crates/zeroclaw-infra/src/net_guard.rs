//! Network-safety primitives shared across crates that must reject SSRF and
//! local/private targets. Lives in `zeroclaw-infra` so both the tool layer
//! (`zeroclaw-tools` domain guard) and its `zeroclaw-channels` consumers read
//! one implementation.
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
//! - [`normalize_egress_pattern`] / [`normalize_egress_patterns`] /
//!   [`egress_host_matches`]: the strict, deny-by-default grammar and matcher
//!   used by plugin egress policy, where the permissive tool-layer semantics
//!   above would be a footgun.
//! - [`normalize_host`]: canonicalize one *request* host, strictly.
//! - [`is_cloud_metadata_ip`], [`is_private_or_local_host`], [`is_non_global_v4`],
//!   [`is_non_global_v6`]: address-class classification.
//! - [`Nat64Prefix`] / [`parse_nat64_prefixes`]: the operator-declared,
//!   network-specific RFC 6052 NAT64 prefixes deployed on this host's network.
//! - [`validate_resolved_ips_are_public`] /
//!   [`validate_resolved_ips_exclude_metadata`]: post-resolution SSRF checks.
//! - [`ResolvedDestination`] / [`PrivateNetworkAccess`] / [`NetworkGuardError`]:
//!   a validated answer set that a caller dials *instead of* resolving again,
//!   with a typed rejection reason.
//!
//! # NAT64 and the validation boundary
//!
//! The address-class predicates are deliberately prefix-unaware: they know only
//! the address forms that are the same on every network (IPv4-mapped, the
//! deprecated IPv4-compatible form, 6to4, and the RFC 6052 *well-known* prefix
//! `64:ff9b::/96`). A *network-specific* NAT64 prefix is chosen per deployment
//! and cannot be inferred from an address, so it is supplied by the caller and
//! consulted by the validators, which are the actual egress boundary.

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
// subdomains. Plugin egress policy requires the opposite defaults,
// so it gets its own grammar and its own matcher rather than a flag on the
// permissive one. Keeping them as separate functions means a caller cannot
// accidentally inherit the loose rules by forgetting an argument.

/// Canonicalize one egress allowlist entry, or explain why it is rejected.
///
/// This is the single grammar for both halves of the egress contract: the signed
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
    if raw.trim() != raw {
        return Err(format!(
            "entry {raw:?} must not have leading or trailing whitespace"
        ));
    }
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

/// Return whether an egress grant contains every destination matched by a
/// carveout pattern.
///
/// Both arguments must already be canonicalized by
/// [`normalize_egress_pattern`]. Exact grants contain only the same exact
/// host. A suffix grant contains exact subdomains and narrower suffix grants,
/// but never its apex. An exact grant can never contain a suffix grant.
#[must_use]
pub fn egress_pattern_contains(grant: &str, carveout: &str) -> bool {
    let (grant_host, grant_is_suffix) = match grant.strip_prefix("*.") {
        Some(host) => (host, true),
        None => (grant, false),
    };
    let (carveout_host, carveout_is_suffix) = match carveout.strip_prefix("*.") {
        Some(host) => (host, true),
        None => (carveout, false),
    };

    match (grant_is_suffix, carveout_is_suffix) {
        (true, true) => {
            carveout_host == grant_host || carveout_host.ends_with(&format!(".{grant_host}"))
        }
        (true, false) => {
            carveout_host.parse::<std::net::IpAddr>().is_err()
                && carveout_host.ends_with(&format!(".{grant_host}"))
        }
        (false, false) => grant_host == carveout_host,
        (false, true) => false,
    }
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
/// An empty `allowed` list matches nothing: no grant means no reach.
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
/// bracketed IPv6 (`[::1]`), ignores DNS root-label dots, and is case-insensitive.
#[must_use]
pub fn is_private_or_local_host(host: &str) -> bool {
    let canonical = host.trim_end_matches('.');
    let bare = canonical
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(canonical)
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
///
/// The classification follows the
/// [IANA IPv4 Special-Purpose Address Registry][iana-v4]. Deprecated
/// translation space is rejected conservatively even where the registry no
/// longer assigns a global-reachability value.
///
/// [iana-v4]: https://www.iana.org/assignments/iana-ipv4-special-registry/iana-ipv4-special-registry.xhtml
#[must_use]
pub fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    a == 0 // 0.0.0.0/8 ("This network")
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b)) // RFC 6598 shared address space
        || a >= 240 // Reserved
        // PCP (192.0.0.9) and TURN (192.0.0.10) anycast are globally routed
        // but terminate on local-network infrastructure, so the SSRF boundary
        // conservatively keeps the entire protocol-assignment block closed.
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2) // Documentation (192.0.2.0/24)
        || (a == 192 && b == 88 && c == 99) // Deprecated 6to4 relay anycast
        || (a == 198 && b == 51 && c == 100) // Documentation (198.51.100.0/24)
        || (a == 203 && b == 0 && c == 113) // Documentation (203.0.113.0/24)
        || (a == 198 && (18..=19).contains(&b)) // Benchmarking (198.18.0.0/15)
}

/// True when an IPv6 address is not globally routable (loopback, ULA,
/// link-local, site-local, documentation, multicast, unallocated/reserved,
/// or an IPv4-embedded non-global v4).
///
/// IANA currently allocates [global IPv6 unicast addresses][iana-v6-space]
/// from `2000::/3`.
/// This classifier additionally handles the globally reachable NAT64
/// well-known prefix and the more-specific exceptions in the
/// [IANA IPv6 Special-Purpose Address Registry][iana-v6-special]. Everything
/// else defaults closed.
///
/// [iana-v6-space]: https://www.iana.org/assignments/ipv6-address-space/ipv6-address-space.xhtml
/// [iana-v6-special]: https://www.iana.org/assignments/iana-ipv6-special-registry/iana-ipv6-special-registry.xhtml
#[must_use]
pub fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();

    // IPv4-mapped addresses and the NAT64 well-known /96 reach the embedded
    // IPv4 destination, so classify that effective destination rather than
    // accepting an encoded private address or rejecting an encoded public one.
    if let Some(v4) = embedded_ipv4(v6) {
        return is_non_global_v4(v4);
    }

    // IANA currently assigns global IPv6 unicast space only from 2000::/3.
    // The one globally reachable special prefix outside it (64:ff9b::/96) was
    // handled as an IPv4-embedded address above.
    if (segs[0] & 0xe000) != 0x2000 {
        return true;
    }

    let ietf_protocol_assignments = segs[0] == 0x2001 && segs[1] < 0x0200;
    // IANA marks these exact anycast assignments globally reachable. Keep
    // them distinct from the enclosing non-global 2001::/23 allocation.
    let globally_reachable_ietf_exception = matches!(
        u128::from_be_bytes(v6.octets()),
        0x2001_0001_0000_0000_0000_0000_0000_0001..=0x2001_0001_0000_0000_0000_0000_0000_0003
    ) || segs[0] == 0x2001 && segs[1] == 0x0003
        || segs[0] == 0x2001 && segs[1] == 0x0004 && segs[2] == 0x0112
        || segs[0] == 0x2001 && (0x0020..=0x003f).contains(&segs[1]);

    (ietf_protocol_assignments && !globally_reachable_ietf_exception)
        || segs[0] == 0x2002 // 6to4: global reachability is not guaranteed
        || (segs[0] == 0x2001 && segs[1] == 0x0db8) // Documentation (2001:db8::/32)
        || (segs[0] == 0x3fff && (segs[1] & 0xf000) == 0) // Documentation (3fff::/20)
}

const ALIBABA_METADATA_V4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(100, 100, 100, 200);
const AZURE_PLATFORM_V4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(168, 63, 129, 16);
const GCP_METADATA_V6: std::net::Ipv6Addr =
    std::net::Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254);

fn embedded_ipv4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return Some(v4);
    }

    match v6.segments() {
        // RFC 6052 well-known prefix 64:ff9b::/96.
        [0x0064, 0xff9b, 0, 0, 0, 0, high, low] => {
            let [a, b] = high.to_be_bytes();
            let [c, d] = low.to_be_bytes();
            Some(std::net::Ipv4Addr::new(a, b, c, d))
        }
        _ => None,
    }
}

fn metadata_embedded_ipv4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    if let Some(v4) = embedded_ipv4(v6) {
        return Some(v4);
    }

    match v6.segments() {
        // Deprecated IPv4-compatible form ::a.b.c.d.
        [0, 0, 0, 0, 0, 0, high, low]
        // 6to4 embeds the effective IPv4 next hop after 2002::/16.
        | [0x2002, high, low, _, _, _, _, _] => {
            let [a, b] = high.to_be_bytes();
            let [c, d] = low.to_be_bytes();
            Some(std::net::Ipv4Addr::new(a, b, c, d))
        }
        _ => None,
    }
}

/// True when `ip` is a known cloud instance-metadata service address.
///
/// The classifier covers the entire IPv4 link-local range used by instance,
/// task, and pod metadata services; the AWS `fd00:ec2::/64` service range;
/// Google Compute Engine IPv6; Alibaba ECS IPv4; and Azure's host-local
/// WireServer address. IPv4-mapped and RFC 6052 well-known NAT64 forms receive
/// the same classification. Metadata services can also use private DNS names
/// or provider-specific addresses, so callers must not treat these ranges as
/// provider discovery.
///
/// Known metadata addresses are refused unconditionally by both
/// [`validate_resolved_ips_are_public`] and
/// [`validate_resolved_ips_exclude_metadata`], so an operator opt-in for
/// private destinations never re-opens them.
#[must_use]
pub fn is_cloud_metadata_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, _, _] = v4.octets();
            (a == 169 && b == 254) || v4 == ALIBABA_METADATA_V4 || v4 == AZURE_PLATFORM_V4
        }
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            (segments[..4] == [0xfd00, 0x0ec2, 0, 0])
                || v6 == GCP_METADATA_V6
                || metadata_embedded_ipv4(v6)
                    .is_some_and(|v4| is_cloud_metadata_ip(std::net::IpAddr::V4(v4)))
        }
    }
}

/// True when `ip` is a provider-documented metadata endpoint, rather than
/// another address in the metadata-sensitive IPv4 link-local range.
///
/// [`is_cloud_metadata_ip`] intentionally blocks all of `169.254.0.0/16`.
/// This narrower classifier exists only so diagnostics can distinguish known
/// metadata endpoints from ordinary APIPA/link-local addresses without
/// weakening that unconditional block. Recognized IPv4-embedded forms receive
/// the same classification as their effective IPv4 destination.
#[must_use]
pub fn is_known_cloud_metadata_endpoint(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            matches!(
                v4.octets(),
                [169, 254, 169, 254] | [169, 254, 170, 2] | [169, 254, 170, 23]
            ) || v4 == ALIBABA_METADATA_V4
                || v4 == AZURE_PLATFORM_V4
        }
        std::net::IpAddr::V6(v6) => {
            let segments = v6.segments();
            (segments[..4] == [0xfd00, 0x0ec2, 0, 0])
                || v6 == GCP_METADATA_V6
                || metadata_embedded_ipv4(v6)
                    .is_some_and(|v4| is_known_cloud_metadata_endpoint(std::net::IpAddr::V4(v4)))
        }
    }
}

// ── network-specific NAT64 prefixes (RFC 6052) ────────────────────
// A NAT64 translator rewrites an IPv6 destination inside its configured
// prefix to the IPv4 address embedded in that destination. The prefix is a
// deployment choice, so an attacker who controls a hostname's DNS answer can
// return an apparently-global IPv6 address that the local translator delivers
// to `10.0.0.1` or `169.254.169.254`. Nothing in the address itself reveals
// this, so operators declare the prefixes their network actually runs and the
// validators classify the embedded destination as well as the raw address.

/// The six prefix lengths RFC 6052 §2.2 defines for IPv4-embedded IPv6
/// addresses. No other length is a valid NAT64 prefix.
const RFC6052_PREFIX_LENGTHS: [u8; 6] = [32, 40, 48, 56, 64, 96];

/// One operator-declared, network-specific NAT64 prefix.
///
/// Construct with [`Nat64Prefix::parse`] (or [`parse_nat64_prefixes`] for a
/// whole configured list); there is no other constructor, so a value of this
/// type is always one of the six RFC 6052 §2.2 prefix lengths with no bits set
/// beyond that length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nat64Prefix {
    network: std::net::Ipv6Addr,
    len: u8,
}

impl std::fmt::Display for Nat64Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.len)
    }
}

impl Nat64Prefix {
    /// Parse one `<ipv6>/<len>` entry, for example `"2001:db8:122:344::/96"`.
    ///
    /// # Errors
    ///
    /// Returns an error describing the problem when the entry has no `/`, does
    /// not parse as an IPv6 address, uses a prefix length outside
    /// RFC 6052 §2.2's `/32`, `/40`, `/48`, `/56`, `/64`, `/96`, or sets any
    /// bit beyond the prefix length.
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let entry = raw.trim();
        if entry.is_empty() {
            anyhow::bail!("NAT64 prefix is empty");
        }

        let (address, length) = entry
            .split_once('/')
            .ok_or_else(|| anyhow::Error::msg("missing '/<prefix-length>'"))?;

        let network = address
            .parse::<std::net::Ipv6Addr>()
            .map_err(|e| anyhow::Error::msg(format!("'{address}' is not an IPv6 address: {e}")))?;
        let len = length
            .parse::<u8>()
            .map_err(|e| anyhow::Error::msg(format!("'{length}' is not a prefix length: {e}")))?;

        if !RFC6052_PREFIX_LENGTHS.contains(&len) {
            anyhow::bail!(
                "prefix length /{len} is not one of the RFC 6052 lengths /32, /40, /48, /56, /64, /96"
            );
        }

        // `len` is at most 96 here, so the shift is always in range.
        let host_bits = u128::MAX >> len;
        if u128::from_be_bytes(network.octets()) & host_bits != 0 {
            anyhow::bail!("address '{address}' sets bits beyond /{len}");
        }

        Ok(Self { network, len })
    }

    /// The prefix length in bits.
    #[must_use]
    pub const fn prefix_len(&self) -> u8 {
        self.len
    }

    /// The prefix network address, with all bits beyond [`Self::prefix_len`]
    /// zero.
    #[must_use]
    pub const fn network(&self) -> std::net::Ipv6Addr {
        self.network
    }

    /// Decode the IPv4 address `v6` embeds under this prefix, or `None` when
    /// `v6` is not inside the prefix.
    ///
    /// The layouts are RFC 6052 §2.2's: the embedded IPv4 octets follow the
    /// prefix and skip octet 8, the "u" octet, for every length below `/96`.
    ///
    /// # The u-octet is decoded regardless of its value
    ///
    /// RFC 6052 requires a *translator* to set octet 8 to zero, and §3.1 says
    /// an address whose u-octet is non-zero is not a valid IPv4-embedded
    /// address. This decoder deliberately ignores that rule, because here the
    /// address comes from a DNS answer the attacker writes, not from a
    /// translator. Requiring `u == 0` would classify
    /// `<prefix>:ff:a:0:100::`-style answers as opaque IPv6 while a permissive
    /// translator still delivered them to the embedded IPv4 destination — a
    /// bypass. Decoding unconditionally can only over-approximate what the
    /// translator reaches, which is the safe direction for a deny boundary.
    #[must_use]
    pub fn embedded_ipv4(&self, v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
        // `len` is at most 96, so the shift is always in range.
        let prefix_mask = !(u128::MAX >> self.len);
        if u128::from_be_bytes(v6.octets()) & prefix_mask
            != u128::from_be_bytes(self.network.octets())
        {
            return None;
        }

        let o = v6.octets();
        let embedded = match self.len {
            32 => [o[4], o[5], o[6], o[7]],
            40 => [o[5], o[6], o[7], o[9]],
            48 => [o[6], o[7], o[9], o[10]],
            56 => [o[7], o[9], o[10], o[11]],
            64 => [o[9], o[10], o[11], o[12]],
            96 => [o[12], o[13], o[14], o[15]],
            // Unreachable: `parse` is the only constructor and restricts `len`
            // to RFC6052_PREFIX_LENGTHS. Returning `None` keeps the guard
            // total instead of panicking if that ever stops holding.
            _ => return None,
        };
        Some(std::net::Ipv4Addr::from(embedded))
    }
}

/// Parse a whole operator-authored NAT64 prefix list, sorted and deduplicated.
///
/// `label` names the configuration surface in the error message so the
/// operator can find the offending entry (for example
/// `"security.nat64_prefixes"`).
///
/// # Errors
///
/// Returns an error naming every rejected entry, with the reason it was
/// rejected, when one or more entries fail [`Nat64Prefix::parse`].
///
/// A malformed list is never silently reduced to its well-formed subset. One
/// bad entry rejects the whole list so that a typo fails the caller closed
/// instead of quietly narrowing the validation boundary — a list that parsed
/// to "no prefixes" would look exactly like a deployment that runs no NAT64
/// translator, and would disable network-specific classification without any
/// signal.
pub fn parse_nat64_prefixes(prefixes: &[String], label: &str) -> anyhow::Result<Vec<Nat64Prefix>> {
    let mut parsed = Vec::with_capacity(prefixes.len());
    let mut rejected = Vec::new();

    for entry in prefixes {
        match Nat64Prefix::parse(entry) {
            Ok(prefix) => parsed.push(prefix),
            Err(err) => rejected.push(format!("'{entry}' ({err})")),
        }
    }

    if !rejected.is_empty() {
        anyhow::bail!(
            "Invalid {label} entry(s): [{}]. Each entry must be an RFC 6052 NAT64 prefix written \
             as <ipv6>/<length> with a length of 32, 40, 48, 56, 64, or 96 and no bits set beyond \
             it, for example \"2001:db8:122:344::/96\".",
            rejected.join(", ")
        );
    }

    parsed.sort_unstable();
    parsed.dedup();
    Ok(parsed)
}

/// Decode `v6` under **every** configured prefix that contains it, yielding
/// each such prefix alongside the IPv4 address it embeds.
///
/// Configured prefixes may overlap: a prefix is a CIDR range, so a declared
/// `/96` can nest inside a declared `/32`, and one IPv6 address then sits in
/// both. The two prefixes decode different octets, so they translate that one
/// address to two *different* IPv4 destinations — for example
/// `2001:67c:5db8:d822:1234:5678:a9fe:a9fe` decodes through `2001:67c::/32` to
/// the global `93.184.216.34`, but through
/// `2001:67c:5db8:d822:1234:5678::/96` to the metadata address
/// `169.254.169.254`.
///
/// Both destinations are reachable, because the operator declared both
/// translations. Stopping at the first containing prefix would therefore let
/// the broader declaration vouch for an address that a more-specific
/// declaration carries somewhere denied, so callers must evaluate every pair
/// this yields and reject the address when *any* translation is denied.
///
/// Yields in the order the prefixes are supplied. For zero or one matching
/// prefix this is exactly the single decode that prefix produces.
fn network_specific_embedded_ipv4s(
    v6: std::net::Ipv6Addr,
    nat64_prefixes: &[Nat64Prefix],
) -> impl Iterator<Item = (Nat64Prefix, std::net::Ipv4Addr)> + '_ {
    nat64_prefixes
        .iter()
        .filter_map(move |prefix| prefix.embedded_ipv4(v6).map(|v4| (*prefix, v4)))
}

fn metadata_block_error(host: &str, ip: std::net::IpAddr) -> anyhow::Error {
    if is_known_cloud_metadata_endpoint(ip) {
        anyhow::Error::msg(format!(
            "Blocked host '{host}' resolved to cloud metadata address {ip}"
        ))
    } else {
        anyhow::Error::msg(format!(
            "Blocked host '{host}' resolved to link-local address {ip}; this range is blocked \
             unconditionally because cloud metadata services are hosted in 169.254.0.0/16"
        ))
    }
}

fn nat64_metadata_block_error(
    host: &str,
    resolved: std::net::Ipv6Addr,
    prefix: Nat64Prefix,
    embedded: std::net::Ipv4Addr,
) -> anyhow::Error {
    if is_known_cloud_metadata_endpoint(std::net::IpAddr::V4(embedded)) {
        anyhow::Error::msg(format!(
            "Blocked host '{host}' resolved to {resolved}, which the configured NAT64 prefix \
             {prefix} translates to cloud metadata address {embedded}"
        ))
    } else {
        anyhow::Error::msg(format!(
            "Blocked host '{host}' resolved to {resolved}, which the configured NAT64 prefix \
             {prefix} translates to link-local address {embedded}; this range is blocked \
             unconditionally because cloud metadata services are hosted in 169.254.0.0/16"
        ))
    }
}

// ── resolved-address validation ───────────────────────────────────
// These helpers only classify the supplied answer. To prevent DNS rebinding,
// callers must connect to the exact addresses they validated rather than
// resolving the hostname again.
//
// Both validators take the deployment's network-specific NAT64 prefixes
// (see [`Nat64Prefix`]). Pass an empty slice when the host runs no NAT64
// translator, or only the well-known `64:ff9b::/96` prefix, which the
// address-class predicates already decode.

/// Reject a resolution that contains any metadata or non-globally-routable
/// address. This is the default post-resolution SSRF check.
///
/// An IPv6 answer inside one of `nat64_prefixes` is classified as the literal
/// address, and again as the IPv4 address the local translator would deliver
/// it to. All of those must be globally routable and none may be a metadata
/// address. When configured prefixes overlap, an answer can sit inside several
/// of them and decode to a *different* IPv4 destination under each; every such
/// destination is reachable, so the answer is rejected when any one of them is
/// denied rather than accepted on the first that happens to be acceptable.
///
/// # DNS pinning
///
/// This function validates only the supplied DNS answer. After it succeeds,
/// the caller must connect to one of these exact validated addresses and must
/// not resolve `host` again; otherwise DNS rebinding can replace the checked
/// destination.
///
/// # Errors
///
/// Returns an error when `ips` is empty, contains a known cloud metadata
/// address, or contains any non-globally-routable address — including one
/// reached through a configured NAT64 prefix.
pub fn validate_resolved_ips_are_public(
    host: &str,
    ips: &[std::net::IpAddr],
    nat64_prefixes: &[Nat64Prefix],
) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        if is_cloud_metadata_ip(*ip) {
            return Err(metadata_block_error(host, *ip));
        }

        if let std::net::IpAddr::V6(v6) = ip {
            // Overlapping prefixes translate one address to several different
            // destinations. Every one of them is reachable, so the address is
            // accepted only when all of them are acceptable.
            for (prefix, embedded) in network_specific_embedded_ipv4s(*v6, nat64_prefixes) {
                if is_cloud_metadata_ip(std::net::IpAddr::V4(embedded)) {
                    return Err(nat64_metadata_block_error(host, *v6, prefix, embedded));
                }
                if is_non_global_v4(embedded) {
                    anyhow::bail!(
                        "Blocked host '{host}' resolved to {v6}, which the configured NAT64 prefix \
                         {prefix} translates to non-global address {embedded}"
                    );
                }
            }
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

/// Reject a resolution that contains a known metadata address, but permit
/// other private and loopback addresses. For callers that carry an explicit
/// operator opt-in for private destinations; the known metadata endpoints
/// remain blocked regardless.
///
/// The private opt-in never extends to metadata addresses, so an IPv6 answer
/// inside one of `nat64_prefixes` is rejected when the IPv4 address it embeds
/// is a metadata address. Overlapping prefixes decode one answer to several
/// destinations; the answer is rejected when any of them is a metadata
/// address.
///
/// # DNS pinning
///
/// This function validates only the supplied DNS answer. After it succeeds,
/// the caller must connect to one of these exact validated addresses and must
/// not resolve `host` again; otherwise DNS rebinding can replace the checked
/// destination.
///
/// # Errors
///
/// Returns an error when `ips` is empty or contains a known cloud metadata
/// address — including one reached through a configured NAT64 prefix.
pub fn validate_resolved_ips_exclude_metadata(
    host: &str,
    ips: &[std::net::IpAddr],
    nat64_prefixes: &[Nat64Prefix],
) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        if is_cloud_metadata_ip(*ip) {
            return Err(metadata_block_error(host, *ip));
        }

        if let std::net::IpAddr::V6(v6) = ip {
            // As in `validate_resolved_ips_are_public`: overlapping prefixes
            // each declare a reachable translation, so any one of them
            // reaching metadata refuses the address.
            for (prefix, embedded) in network_specific_embedded_ipv4s(*v6, nat64_prefixes) {
                if is_cloud_metadata_ip(std::net::IpAddr::V4(embedded)) {
                    return Err(nat64_metadata_block_error(host, *v6, prefix, embedded));
                }
            }
        }
    }

    Ok(())
}

// ── pinned destinations ───────────────────────────────────────────
// The validators above answer "is this answer set safe". A caller that then
// dials the *hostname* has thrown that answer away and reopened the rebinding
// window the validation closed. `ResolvedDestination` makes the pinning part
// of the type: constructing one runs the validation, and the only thing it
// hands back is the exact address list that passed.
//
// The verdict is delegated to the two validators above rather than restated
// here, so a plugin transport and a built-in tool cannot disagree about what
// is reachable — including through a configured NAT64 prefix.

/// Whether an authorized destination may resolve to private/local addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateNetworkAccess {
    /// Every resolved address must be globally routable.
    Deny,
    /// An all-private answer set is accepted, except metadata addresses.
    Allow,
}

/// Why a host or its resolved address set is unsafe to dial.
///
/// This is the typed sibling of the validators' `anyhow` errors, for callers
/// that must branch on the reason rather than only report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkGuardError {
    /// The host is empty, malformed, or not a canonical DNS name/IP literal.
    InvalidHost(String),
    /// Port zero is never a valid outbound destination.
    InvalidPort,
    /// DNS returned no addresses.
    NoAddresses { host: String, port: u16 },
    /// A resolver returned an address for a different port.
    PortMismatch {
        expected: u16,
        address: std::net::SocketAddr,
    },
    /// A literal IP resolved to a different IP.
    LiteralMismatch {
        literal: std::net::IpAddr,
        address: std::net::SocketAddr,
    },
    /// The answer set reaches a cloud metadata endpoint, directly or through a
    /// configured NAT64 prefix. Never allowed, whatever the private opt-in.
    CloudMetadata { host: String, reason: String },
    /// A syntactically local hostname was not explicitly authorized.
    PrivateHostDenied(String),
    /// A local hostname unexpectedly resolved only to public addresses.
    PrivateHostResolvedPublic(String),
    /// The answer set contains a non-globally-routable address and private
    /// access was not authorized for this host.
    PrivateNetworkDenied { host: String, reason: String },
    /// DNS returned both globally routable and private/local addresses.
    MixedAddressClasses,
}

impl std::fmt::Display for NetworkGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHost(host) => write!(f, "invalid outbound host {host:?}"),
            Self::InvalidPort => f.write_str("outbound port must be greater than zero"),
            Self::NoAddresses { host, port } => {
                write!(f, "DNS resolution for {host}:{port} returned no addresses")
            }
            Self::PortMismatch { expected, address } => write!(
                f,
                "resolved address {address} does not use requested port {expected}"
            ),
            Self::LiteralMismatch { literal, address } => write!(
                f,
                "IP literal {literal} resolved to a different address {address}"
            ),
            Self::CloudMetadata { reason, .. } | Self::PrivateNetworkDenied { reason, .. } => {
                f.write_str(reason)
            }
            Self::PrivateHostDenied(host) => {
                write!(f, "private or local host {host:?} is not authorized")
            }
            Self::PrivateHostResolvedPublic(host) => write!(
                f,
                "private or local host {host:?} resolved to public address space"
            ),
            Self::MixedAddressClasses => {
                f.write_str("DNS resolution returned a mixed public/private address set")
            }
        }
    }
}

impl std::error::Error for NetworkGuardError {}

/// Normalize a DNS host or IP literal for policy matching and SNI selection.
///
/// Deliberately stricter than [`normalize_domain`], which exists to be
/// forgiving about how an operator *wrote* an allowlist entry and will happily
/// parse a whole URL down to its host. A request host is not operator prose: it
/// arrives from a guest or an adapter, so anything carrying a scheme, userinfo,
/// path, or port is a malformed destination rather than something to rewrite.
/// DNS names are ASCII-only; internationalized names must arrive as punycode.
/// IPv6 brackets are accepted and removed, and a single trailing root dot is
/// stripped.
///
/// # Errors
///
/// Returns [`NetworkGuardError::InvalidHost`] for a malformed host.
pub fn normalize_host(host: &str) -> Result<String, NetworkGuardError> {
    normalize_request_host(host).ok_or_else(|| NetworkGuardError::InvalidHost(host.to_string()))
}

fn normalize_request_host(host: &str) -> Option<String> {
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

/// The trust classes one answer address can land in: `(reaches_global,
/// reaches_non_global)` across the raw address itself and every configured
/// NAT64 translation that contains it.
///
/// This is the *trust-zone* question, not the deny question: it exists so a
/// caller can tell whether one answer set spans two zones. Overlapping
/// declared prefixes can translate a single IPv6 address to destinations in
/// different zones, and the raw address is itself one of the interpretations,
/// so the answer must be the set of classes observed rather than one
/// collapsed verdict; which interpretation carries the connection is route
/// selection's choice, not this module's. Whether an address may be dialed at
/// all is decided by the validators.
fn effective_trust_classes(ip: std::net::IpAddr, nat64_prefixes: &[Nat64Prefix]) -> (bool, bool) {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let non_global = is_non_global_v4(v4);
            (!non_global, non_global)
        }
        std::net::IpAddr::V6(v6) => {
            let mut reaches_non_global = is_non_global_v6(v6);
            let mut reaches_global = !reaches_non_global;
            for (_, embedded) in network_specific_embedded_ipv4s(v6, nat64_prefixes) {
                if is_non_global_v4(embedded) {
                    reaches_non_global = true;
                } else {
                    reaches_global = true;
                }
            }
            (reaches_global, reaches_non_global)
        }
    }
}

/// A normalized host and the exact address set that passed network policy.
///
/// Callers must dial [`Self::addresses`] directly. Resolving [`Self::host`]
/// again would reopen the DNS-rebinding window this type closes, so the type
/// deliberately offers no way to get from a value back to a fresh resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDestination {
    host: String,
    port: u16,
    addresses: Vec<std::net::SocketAddr>,
}

impl ResolvedDestination {
    /// Validate one DNS result and retain the exact addresses to dial.
    ///
    /// `nat64_prefixes` is the deployment's `security.nat64_prefixes`, parsed
    /// once by the caller at construction. Passing an empty slice states that
    /// the host runs no network-specific translator; it does not skip the
    /// well-known `64:ff9b::/96` form, which the address predicates decode
    /// unconditionally.
    ///
    /// Three things happen here that a bare validator call does not do:
    ///
    /// 1. Cloud metadata is refused for **both** access modes, so an operator
    ///    opt-in for private destinations never re-opens it.
    /// 2. A mixed public/private answer set is refused even when private
    ///    access is authorized. Otherwise resolver ordering or connection
    ///    fallback silently decides which trust zone the connection lands in.
    /// 3. The surviving addresses are retained, which is the pin.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkGuardError`] for a malformed host or port, an empty or
    /// mismatched answer set, a metadata endpoint, an unauthorized private
    /// address, or an answer spanning both address classes.
    pub fn new(
        host: &str,
        port: u16,
        addresses: impl IntoIterator<Item = std::net::SocketAddr>,
        private_access: PrivateNetworkAccess,
        nat64_prefixes: &[Nat64Prefix],
    ) -> Result<Self, NetworkGuardError> {
        let host = normalize_host(host)?;
        if port == 0 {
            return Err(NetworkGuardError::InvalidPort);
        }

        let literal = host.parse::<std::net::IpAddr>().ok();
        let private_host = is_private_or_local_host(&host);
        if private_host && private_access == PrivateNetworkAccess::Deny {
            return Err(NetworkGuardError::PrivateHostDenied(host));
        }

        let mut unique = std::collections::HashSet::new();
        let mut addresses = addresses
            .into_iter()
            .filter(|address| unique.insert(*address))
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(NetworkGuardError::NoAddresses { host, port });
        }

        for address in &addresses {
            if address.port() != port {
                return Err(NetworkGuardError::PortMismatch {
                    expected: port,
                    address: *address,
                });
            }
            if let Some(literal) = literal
                && literal != address.ip()
            {
                return Err(NetworkGuardError::LiteralMismatch {
                    literal,
                    address: *address,
                });
            }
        }

        let ips = addresses
            .iter()
            .map(std::net::SocketAddr::ip)
            .collect::<Vec<_>>();

        // Metadata is refused in both modes, and this is the only place that
        // decision is made for this path: under `Allow` no other check runs.
        validate_resolved_ips_exclude_metadata(&host, &ips, nat64_prefixes).map_err(|error| {
            NetworkGuardError::CloudMetadata {
                host: host.clone(),
                reason: error.to_string(),
            }
        })?;

        if private_access == PrivateNetworkAccess::Deny {
            validate_resolved_ips_are_public(&host, &ips, nat64_prefixes).map_err(|error| {
                NetworkGuardError::PrivateNetworkDenied {
                    host: host.clone(),
                    reason: error.to_string(),
                }
            })?;
        }

        let mut saw_public = false;
        let mut saw_private = false;
        for ip in &ips {
            let (reaches_global, reaches_non_global) = effective_trust_classes(*ip, nat64_prefixes);
            saw_public |= reaches_global;
            saw_private |= reaches_non_global;
        }
        if saw_public && saw_private {
            return Err(NetworkGuardError::MixedAddressClasses);
        }
        if private_host && saw_public {
            return Err(NetworkGuardError::PrivateHostResolvedPublic(host));
        }

        // Resolver order is useful for connection fallback, so deduplicate
        // without sorting.
        addresses.shrink_to_fit();
        Ok(Self {
            host,
            port,
            addresses,
        })
    }

    /// Canonical lowercase host, without IPv6 brackets or a trailing DNS dot.
    /// Use this for SNI and certificate verification, never for a second
    /// resolution.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Authorized destination port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Exact validated socket addresses. Dial these instead of resolving again.
    #[must_use]
    pub fn addresses(&self) -> &[std::net::SocketAddr] {
        &self.addresses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

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
        assert!(normalize_egress_pattern(" example.com").is_err());
        assert!(normalize_egress_pattern("example.com ").is_err());
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
    fn egress_pattern_containment_preserves_apex_and_wildcard_boundaries() {
        let cases = [
            ("example.com", "example.com", true),
            ("example.com", "api.example.com", false),
            ("example.com", "*.example.com", false),
            ("*.example.com", "example.com", false),
            ("*.example.com", "api.example.com", true),
            ("*.example.com", "*.example.com", true),
            ("*.example.com", "*.api.example.com", true),
            ("*.api.example.com", "*.example.com", false),
            ("*.example.com", "api.other.example.com", true),
            ("1.1.1.1", "1.1.1.1", true),
            ("*.example.com", "1.1.1.1", false),
        ];
        for (grant, carveout, expected) in cases {
            assert_eq!(
                egress_pattern_contains(grant, carveout),
                expected,
                "grant {grant:?}, carveout {carveout:?}"
            );
        }
        let bracketed = normalize_egress_pattern("[::1]").unwrap();
        assert_eq!(bracketed, "::1");
        assert!(egress_pattern_contains(&bracketed, &bracketed));
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
    fn ipv4_special_purpose_boundaries_follow_registry() {
        for address in [
            "0.0.0.1",
            "0.255.255.255",
            "100.64.0.1",
            "192.0.0.8",
            "192.0.0.9",
            "192.0.0.10",
            "192.0.0.11",
            "192.88.99.1",
            "198.19.255.255",
            "240.0.0.1",
        ] {
            let address = address.parse::<Ipv4Addr>().unwrap();
            assert!(is_non_global_v4(address), "{address} must be blocked");
        }

        for address in ["1.0.0.0", "100.128.0.0", "192.88.98.255", "192.88.100.0"] {
            let address = address.parse::<Ipv4Addr>().unwrap();
            assert!(!is_non_global_v4(address), "{address} must be allowed");
        }

        for address in [
            "198.51.100.0",
            "198.51.100.255",
            "203.0.113.0",
            "203.0.113.255",
        ] {
            let address = address.parse::<Ipv4Addr>().unwrap();
            assert!(is_non_global_v4(address), "{address} must be blocked");
        }

        for address in [
            "198.51.99.255",
            "198.51.101.0",
            "203.0.112.255",
            "203.0.114.0",
        ] {
            let address = address.parse::<Ipv4Addr>().unwrap();
            assert!(!is_non_global_v4(address), "{address} must be allowed");
        }
    }

    #[test]
    fn ipv6_special_purpose_and_reserved_ranges_follow_registry() {
        for address in [
            "64:ff9b::10.0.0.1",
            "64:ff9b:1::1",
            "100::1",
            "100:0:0:1::1",
            "2001::1",
            "2001:2::1",
            "2001:10::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
            "4000::1",
            "5f00::1",
            "fec0::1",
        ] {
            let address = address.parse::<Ipv6Addr>().unwrap();
            assert!(is_non_global_v6(address), "{address} must be blocked");
        }

        for address in [
            "64:ff9b::1.1.1.1",
            "2001:1::1",
            "2001:1::2",
            "2001:1::3",
            "2001:3::1",
            "2001:4:112::1",
            "2001:20::1",
            "2001:30::1",
            "2606:4700:4700::1111",
            "2620:4f:8000::1",
        ] {
            let address = address.parse::<Ipv6Addr>().unwrap();
            assert!(!is_non_global_v6(address), "{address} must be allowed");
        }
    }

    #[test]
    fn cloud_metadata_detection_covers_known_provider_and_embedded_addresses() {
        for address in [
            "169.254.0.0",
            "169.254.169.254",
            "169.254.170.2",
            "169.254.170.23",
            "169.254.255.255",
            "100.100.100.200",
            "168.63.129.16",
            "::ffff:169.254.169.254",
            "::ffff:168.63.129.16",
            "::ffff:100.100.100.200",
            "64:ff9b::169.254.169.254",
            "64:ff9b::168.63.129.16",
            "64:ff9b::100.100.100.200",
            "::169.254.169.254",
            "2002:a9fe:a9fe::",
            "fd00:ec2::",
            "fd00:ec2::23",
            "fd00:ec2::254",
            "fd00:ec2:0:0:ffff:ffff:ffff:ffff",
            "fd20:ce::254",
        ] {
            let address = address.parse().unwrap();
            assert!(
                is_cloud_metadata_ip(address),
                "known metadata endpoint {address} must be blocked"
            );
        }

        for address in [
            "169.253.255.255",
            "169.255.0.0",
            "100.100.100.199",
            "100.100.100.201",
            "168.63.129.15",
            "168.63.129.17",
            "fd00:ec1:ffff:ffff:ffff:ffff:ffff:ffff",
            "fd00:ec2:0:1::",
            "fd20:ce::253",
            "fd20:ce::255",
            "::169.253.169.254",
            "2002:a9fd:a9fe::",
        ] {
            let address = address.parse().unwrap();
            assert!(
                !is_cloud_metadata_ip(address),
                "neighboring non-metadata address {address} must not match"
            );
        }
    }

    #[test]
    fn known_metadata_endpoint_detection_excludes_plain_apipa() {
        for address in [
            "169.254.169.254",
            "169.254.170.2",
            "169.254.170.23",
            "100.100.100.200",
            "168.63.129.16",
            "::ffff:169.254.169.254",
            "64:ff9b::169.254.170.2",
            "::169.254.170.23",
            "2002:a9fe:a9fe::",
            "fd00:ec2::23",
            "fd20:ce::254",
        ] {
            let address = address.parse().unwrap();
            assert!(
                is_known_cloud_metadata_endpoint(address),
                "known endpoint {address} must use metadata diagnostics"
            );
        }

        for address in [
            "169.254.0.1",
            "169.254.12.7",
            "169.254.255.254",
            "::ffff:169.254.12.7",
            "64:ff9b::169.254.12.7",
            "::169.254.12.7",
            "2002:a9fe:0c07::",
        ] {
            let address = address.parse().unwrap();
            assert!(is_cloud_metadata_ip(address));
            assert!(
                !is_known_cloud_metadata_endpoint(address),
                "plain link-local address {address} must use APIPA diagnostics"
            );
        }
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
        assert!(is_private_or_local_host("localhost."));
        assert!(is_private_or_local_host("printer.local."));
        assert!(is_private_or_local_host("127.0.0.1."));
        assert!(is_private_or_local_host("192.168.1.1.."));
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
        let err = validate_resolved_ips_are_public("example.com", &ips, &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-global address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_blocks_all_audited_non_global_classes() {
        for address in [
            "0.0.0.1",
            "192.88.99.1",
            "64:ff9b::10.0.0.1",
            "64:ff9b:1::1",
            "100::1",
            "2001:2::1",
            "2002::1",
            "3fff::1",
            "5f00::1",
            "fec0::1",
        ] {
            let ips = [address.parse().unwrap()];
            let err = validate_resolved_ips_are_public("example.test", &ips, &[])
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("non-global address"),
                "{address} produced unexpected error: {err}"
            );
        }
    }

    #[test]
    fn validate_resolved_ips_blocks_metadata_even_for_private_opt_in() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            169, 254, 169, 254,
        ))];
        let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips, &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_labels_plain_apipa_as_link_local() {
        for validate in [
            validate_resolved_ips_are_public
                as fn(&str, &[std::net::IpAddr], &[Nat64Prefix]) -> anyhow::Result<()>,
            validate_resolved_ips_exclude_metadata,
        ] {
            let ips = ["169.254.12.7".parse().unwrap()];
            let err = validate("printer.local", &ips, &[])
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("link-local address"),
                "unexpected error: {err}"
            );
            assert!(err.contains("blocked unconditionally"));
            assert!(!err.contains("cloud metadata address"));
        }
    }

    #[test]
    fn validate_resolved_ips_blocks_mapped_metadata_even_for_private_opt_in() {
        let ips = ["::ffff:169.254.169.254".parse().unwrap()];
        let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips, &[])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_blocks_provider_metadata_even_for_private_opt_in() {
        for address in [
            "169.254.170.2",
            "169.254.170.23",
            "100.100.100.200",
            "168.63.129.16",
            "::ffff:100.100.100.200",
            "::ffff:168.63.129.16",
            "64:ff9b::100.100.100.200",
            "64:ff9b::168.63.129.16",
            "::169.254.169.254",
            "2002:a9fe:a9fe::",
            "fd00:ec2::23",
            "fd20:ce::254",
        ] {
            let ips = [address.parse().unwrap()];
            let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips, &[])
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("cloud metadata address"),
                "{address} produced unexpected error: {err}"
            );
        }
    }

    #[test]
    fn validate_resolved_ips_blocks_ec2_ipv6_metadata_even_for_private_opt_in() {
        let ips = ["fd00:ec2::254".parse().unwrap()];
        let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips, &[])
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
        let err = validate_resolved_ips_are_public("metadata.test", &ips, &[])
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

    // ── network-specific NAT64 prefixes (RFC 6052) ────────────────

    /// RFC 6052 §2.4's own worked example: 192.0.2.33 encoded under one
    /// prefix of each defined length. Decoding each back to 192.0.2.33 is the
    /// conformance test for the octet layouts in [`Nat64Prefix::embedded_ipv4`].
    const RFC6052_SECTION_2_4_VECTORS: [(&str, &str); 6] = [
        ("2001:db8::/32", "2001:db8:c000:221::"),
        ("2001:db8:100::/40", "2001:db8:1c0:2:21::"),
        ("2001:db8:122::/48", "2001:db8:122:c000:2:2100::"),
        ("2001:db8:122:300::/56", "2001:db8:122:3c0:0:221::"),
        ("2001:db8:122:344::/64", "2001:db8:122:344:c0:2:2100::"),
        ("2001:db8:122:344::/96", "2001:db8:122:344::c000:221"),
    ];

    /// The prefix the reviewer named for the private/metadata regressions.
    /// Lives in the IPv6 documentation range, so `is_non_global_v6` already
    /// rejects it for the *public* validator; it exercises the metadata
    /// validator, where no non-global classification runs.
    const DOC_NAT64_96: &str = "2001:db8:122:344::/96";

    /// A globally-classified NAT64 prefix, the shape a real deployment uses.
    /// Needed for the public-validator regressions: under a documentation
    /// prefix that validator rejects for an unrelated reason, so a regression
    /// written there would pass with the NAT64 decode removed.
    const GLOBAL_NAT64_96: &str = "2001:67c:2b0:db32:0:1::/96";
    const GLOBAL_NAT64_64: &str = "2001:67c:2b0:db32::/64";

    /// A broad prefix and two more-specific prefixes nested inside it. One
    /// IPv6 address can sit in the broad prefix and in one of the /96s at the
    /// same time, and the two translations decode to different IPv4
    /// destinations, so an address is only safe when *every* configured
    /// translation that could carry it decodes to an acceptable destination.
    const OVERLAP_BROAD_32: &str = "2001:67c::/32";
    /// Nested in [`OVERLAP_BROAD_32`]; positioned so the broad prefix decodes
    /// the shared address globally.
    const OVERLAP_SPECIFIC_96: &str = "2001:67c:5db8:d822:1234:5678::/96";
    /// Nested in [`OVERLAP_BROAD_32`]; positioned so the broad prefix decodes
    /// the shared address to metadata instead.
    const OVERLAP_MIRROR_SPECIFIC_96: &str = "2001:67c:a9fe:a9fe:1234:5678::/96";

    fn nat64(prefix: &str) -> Vec<Nat64Prefix> {
        parse_nat64_prefixes(&[prefix.to_string()], "test.nat64_prefixes").unwrap()
    }

    /// Build a prefix list in exactly the order given, bypassing the sort in
    /// [`parse_nat64_prefixes`]. Both validators take a plain slice, so their
    /// contract must not depend on the order the caller supplies; the overlap
    /// regressions assert both orders to pin that.
    fn nat64_in_order(prefixes: &[&str]) -> Vec<Nat64Prefix> {
        prefixes
            .iter()
            .map(|entry| Nat64Prefix::parse(entry).unwrap())
            .collect()
    }

    /// Every ordering of an overlapping pair, so a regression cannot pass by
    /// happening to inspect the denied prefix first.
    fn both_orders(a: &str, b: &str) -> [Vec<Nat64Prefix>; 2] {
        [nat64_in_order(&[a, b]), nat64_in_order(&[b, a])]
    }

    fn ip(address: &str) -> std::net::IpAddr {
        address.parse().unwrap()
    }

    #[test]
    fn nat64_prefix_decodes_rfc6052_section_2_4_vectors() {
        let expected = Ipv4Addr::new(192, 0, 2, 33);
        for (prefix, encoded) in RFC6052_SECTION_2_4_VECTORS {
            let parsed = Nat64Prefix::parse(prefix).unwrap();
            let address = encoded.parse::<Ipv6Addr>().unwrap();
            assert_eq!(
                parsed.embedded_ipv4(address),
                Some(expected),
                "RFC 6052 §2.4 vector {encoded} under {prefix} must decode to {expected}"
            );
        }
    }

    #[test]
    fn nat64_prefix_parse_accepts_every_rfc6052_length() {
        for (prefix, _) in RFC6052_SECTION_2_4_VECTORS {
            let parsed = Nat64Prefix::parse(prefix).unwrap();
            assert_eq!(parsed.to_string(), prefix);
        }
        let parsed = Nat64Prefix::parse("  2001:db8:122:344::/96  ").unwrap();
        assert_eq!(parsed.prefix_len(), 96);
        assert_eq!(
            parsed.network(),
            "2001:db8:122:344::".parse::<Ipv6Addr>().unwrap()
        );
    }

    #[test]
    fn nat64_prefix_parse_rejects_lengths_outside_rfc6052() {
        for prefix in [
            "2001:db8::/0",
            "2001:db8::/24",
            "2001:db8::/33",
            "2001:db8::/47",
            "2001:db8::/72",
            "2001:db8::/80",
            "2001:db8::/97",
            "2001:db8::/128",
            "2001:db8::/255",
        ] {
            let err = Nat64Prefix::parse(prefix).unwrap_err().to_string();
            assert!(
                err.contains("RFC 6052 lengths") || err.contains("not a prefix length"),
                "{prefix} produced unexpected error: {err}"
            );
        }
    }

    #[test]
    fn nat64_prefix_parse_rejects_bits_beyond_the_prefix_length() {
        for prefix in [
            "2001:db8:c000::/32",
            "2001:db8:1c0::/40",
            "2001:db8:122:c000::/48",
            "2001:db8:122:3c0::/56",
            "2001:db8:122:344:c0::/64",
            "2001:db8:122:344::c000:221/96",
        ] {
            let err = Nat64Prefix::parse(prefix).unwrap_err().to_string();
            assert!(
                err.contains("sets bits beyond"),
                "{prefix} produced unexpected error: {err}"
            );
        }
    }

    #[test]
    fn nat64_prefix_parse_rejects_malformed_entries() {
        for prefix in [
            "",
            "   ",
            "2001:db8:122:344::",
            "/96",
            "not-an-address/96",
            "10.0.0.0/96",
            "2001:db8:122:344::/",
            "2001:db8:122:344::/ninety-six",
            "2001:db8:122:344::/96/96",
            "[2001:db8:122:344::]/96",
        ] {
            assert!(
                Nat64Prefix::parse(prefix).is_err(),
                "malformed entry {prefix:?} must be rejected"
            );
        }
    }

    #[test]
    fn parse_nat64_prefixes_rejects_the_whole_list_when_any_entry_is_malformed() {
        // Regression for the sibling-PR bug where a filter_map silently
        // dropped malformed entries: one typo emptied the list and disabled
        // network-specific NAT64 classification with no signal. Malformed
        // configuration must fail closed, never degrade.
        let err = parse_nat64_prefixes(
            &[
                "2001:db8:122:344::/96".to_string(),
                "2001:db8::/33".to_string(),
            ],
            "security.nat64_prefixes",
        )
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("Invalid security.nat64_prefixes entry"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains("2001:db8::/33"),
            "error must name the rejected entry: {err}"
        );
    }

    #[test]
    fn parse_nat64_prefixes_accepts_empty_and_deduplicates() {
        assert!(
            parse_nat64_prefixes(&[], "security.nat64_prefixes")
                .unwrap()
                .is_empty()
        );

        let parsed = parse_nat64_prefixes(
            &[
                "2001:db8:122:344::/96".to_string(),
                "2001:db8:122:344::/96".to_string(),
                "2001:db8::/32".to_string(),
            ],
            "security.nat64_prefixes",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn nat64_prefix_decode_ignores_the_u_octet_value() {
        // RFC 6052 tells translators to zero octet 8, but the DNS answer is
        // attacker-authored, not translator-authored. Requiring u == 0 would
        // let a non-zero u-octet slip past classification while a permissive
        // translator still delivered the packet to the embedded IPv4.
        let prefix = Nat64Prefix::parse(GLOBAL_NAT64_64).unwrap();
        let expected = Ipv4Addr::new(10, 0, 0, 1);

        let zero_u = "2001:67c:2b0:db32:a:0:100:0".parse::<Ipv6Addr>().unwrap();
        assert_eq!(prefix.embedded_ipv4(zero_u), Some(expected));

        for nonzero_u in [
            "2001:67c:2b0:db32:ff0a:0:100:0",
            "2001:67c:2b0:db32:100a:0:100:0",
            "2001:67c:2b0:db32:10a:0:100:0",
        ] {
            let address = nonzero_u.parse::<Ipv6Addr>().unwrap();
            assert_eq!(
                prefix.embedded_ipv4(address),
                Some(expected),
                "{nonzero_u} must decode to {expected} regardless of the u-octet"
            );
        }
    }

    #[test]
    fn nat64_prefix_does_not_decode_addresses_outside_the_prefix() {
        let prefix = Nat64Prefix::parse(GLOBAL_NAT64_96).unwrap();
        for outside in [
            "2001:67c:2b0:db33:0:1:a00:1",
            "2001:67c:2b0:db32:0:2:a00:1",
            "2606:4700:4700::1111",
            "64:ff9b::10.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            let address = outside.parse::<Ipv6Addr>().unwrap();
            assert_eq!(
                prefix.embedded_ipv4(address),
                None,
                "{outside} is outside {prefix} and must not decode"
            );
        }
    }

    #[test]
    fn nat64_prefixes_do_not_change_prefix_unaware_classification() {
        // The predicates stay pure: only the validators consult the prefixes.
        let inside = "2001:67c:2b0:db32:0:1:a00:1".parse::<Ipv6Addr>().unwrap();
        assert!(!is_non_global_v6(inside));
        assert!(!is_cloud_metadata_ip(std::net::IpAddr::V6(inside)));

        let metadata = "2001:67c:2b0:db32:0:1:a9fe:a9fe"
            .parse::<Ipv6Addr>()
            .unwrap();
        assert!(!is_cloud_metadata_ip(std::net::IpAddr::V6(metadata)));
    }

    #[test]
    fn validate_resolved_ips_are_public_blocks_private_v4_behind_network_specific_prefix() {
        // The attack: an attacker-controlled name resolves to an
        // apparently-global IPv6 address that the deployment's NAT64
        // translator delivers to 10.0.0.1.
        for (prefix, address) in [
            (GLOBAL_NAT64_96, "2001:67c:2b0:db32:0:1:a00:1"),
            (GLOBAL_NAT64_64, "2001:67c:2b0:db32:a:0:100:0"),
        ] {
            let ips = [ip(address)];
            let err = validate_resolved_ips_are_public("attacker.test", &ips, &nat64(prefix))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("non-global address 10.0.0.1"),
                "{address} under {prefix} produced unexpected error: {err}"
            );
            assert!(err.contains(prefix), "error must name the prefix: {err}");
        }
    }

    #[test]
    fn validate_resolved_ips_are_public_blocks_metadata_v4_behind_network_specific_prefix() {
        let ips = [ip("2001:67c:2b0:db32:0:1:a9fe:a9fe")];
        let err = validate_resolved_ips_are_public("attacker.test", &ips, &nat64(GLOBAL_NAT64_96))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cloud metadata address 169.254.169.254"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_ips_exclude_metadata_blocks_metadata_v4_behind_network_specific_prefix() {
        // The private opt-in never re-opens metadata, so this holds under the
        // reviewer's documentation-range prefix too: this validator runs no
        // non-global classification that could mask the NAT64 decode.
        for prefix in [DOC_NAT64_96, GLOBAL_NAT64_96] {
            let address = if prefix == DOC_NAT64_96 {
                "2001:db8:122:344::a9fe:a9fe"
            } else {
                "2001:67c:2b0:db32:0:1:a9fe:a9fe"
            };
            let ips = [ip(address)];
            let err = validate_resolved_ips_exclude_metadata("metadata.test", &ips, &nat64(prefix))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("cloud metadata address 169.254.169.254"),
                "{address} under {prefix} produced unexpected error: {err}"
            );
            assert!(err.contains(prefix), "error must name the prefix: {err}");
        }
    }

    #[test]
    fn validate_resolved_ips_exclude_metadata_blocks_plain_link_local_behind_prefix() {
        let ips = [ip("2001:67c:2b0:db32:0:1:a9fe:c07")];
        let err =
            validate_resolved_ips_exclude_metadata("printer.test", &ips, &nat64(GLOBAL_NAT64_96))
                .unwrap_err()
                .to_string();
        assert!(err.contains("link-local address 169.254.12.7"), "{err}");
        assert!(err.contains("blocked unconditionally"), "{err}");
        assert!(!err.contains("cloud metadata address"), "{err}");
    }

    #[test]
    fn network_specific_nat64_addresses_pass_when_no_prefix_is_configured() {
        // Honest boundary documentation: without the operator declaring the
        // prefix, nothing in these addresses marks them as NAT64, and both
        // validators accept them. This is the hole the config key closes, and
        // the reason the key exists at all.
        let private_behind_global_prefix = [ip("2001:67c:2b0:db32:0:1:a00:1")];
        assert!(
            validate_resolved_ips_are_public("attacker.test", &private_behind_global_prefix, &[])
                .is_ok()
        );
        assert!(
            validate_resolved_ips_exclude_metadata(
                "attacker.test",
                &private_behind_global_prefix,
                &[]
            )
            .is_ok()
        );

        let metadata_behind_doc_prefix = [ip("2001:db8:122:344::a9fe:a9fe")];
        assert!(
            validate_resolved_ips_exclude_metadata(
                "metadata.test",
                &metadata_behind_doc_prefix,
                &[]
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_resolved_ips_are_public_allows_global_v4_behind_network_specific_prefix() {
        // 93.184.216.34 is a genuinely global IPv4. RFC 6052's own example
        // address, 192.0.2.33, is documentation space and non-global, so it
        // cannot be used to show an accepted destination.
        assert!(!is_non_global_v4(Ipv4Addr::new(93, 184, 216, 34)));
        let ips = [ip("2001:67c:2b0:db32:0:1:5db8:d822")];
        assert!(
            validate_resolved_ips_are_public("cdn.test", &ips, &nat64(GLOBAL_NAT64_96)).is_ok()
        );
    }

    #[test]
    fn overlapping_prefixes_are_sorted_broad_first_by_the_config_parser() {
        // The premise the overlap regressions rest on: the canonical parsed
        // list is sorted by network then prefix length, so the broad prefix is
        // inspected first. Any check that stopped at the first containing
        // prefix would therefore decide on the broad translation and never see
        // the more-specific one the operator also declared.
        let parsed = parse_nat64_prefixes(
            &[
                OVERLAP_SPECIFIC_96.to_string(),
                OVERLAP_BROAD_32.to_string(),
            ],
            "test.nat64_prefixes",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].to_string(), "2001:67c::/32");
        assert_eq!(parsed[0].prefix_len(), 32);
        assert_eq!(parsed[1].prefix_len(), 96);
    }

    #[test]
    fn overlapping_prefixes_reject_when_a_specific_translation_is_metadata() {
        // The reviewer's example. 2001:67c:5db8:d822:1234:5678:a9fe:a9fe is
        // inside both configured prefixes: the /32 decodes it to the global
        // 93.184.216.34, while the /96 decodes it to 169.254.169.254. The
        // deployment declared both translations, so the metadata one is
        // reachable and the address must be refused by both validators.
        let address = "2001:67c:5db8:d822:1234:5678:a9fe:a9fe";
        let ips = [ip(address)];

        // The raw address is unremarkable: only the configured translation
        // makes it dangerous.
        assert!(!is_non_global_v6(address.parse::<Ipv6Addr>().unwrap()));
        assert!(!is_cloud_metadata_ip(ips[0]));

        for prefixes in both_orders(OVERLAP_BROAD_32, OVERLAP_SPECIFIC_96) {
            let order: Vec<String> = prefixes.iter().map(ToString::to_string).collect();

            let err = validate_resolved_ips_are_public("attacker.test", &ips, &prefixes)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("cloud metadata address 169.254.169.254"),
                "are_public with prefixes {order:?} produced unexpected error: {err}"
            );
            assert!(
                err.contains(OVERLAP_SPECIFIC_96),
                "error must name the prefix whose translation was denied: {err}"
            );

            let err = validate_resolved_ips_exclude_metadata("attacker.test", &ips, &prefixes)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("cloud metadata address 169.254.169.254"),
                "exclude_metadata with prefixes {order:?} produced unexpected error: {err}"
            );
            assert!(
                err.contains(OVERLAP_SPECIFIC_96),
                "error must name the prefix whose translation was denied: {err}"
            );
        }
    }

    #[test]
    fn overlapping_prefixes_reject_when_a_specific_translation_is_private() {
        // Same overlap shape, but the more-specific translation reaches
        // RFC 1918 space rather than metadata. Only the public validator
        // refuses private destinations, so this case is asserted there alone.
        let ips = [ip("2001:67c:5db8:d822:1234:5678:a00:1")];

        for prefixes in both_orders(OVERLAP_BROAD_32, OVERLAP_SPECIFIC_96) {
            let order: Vec<String> = prefixes.iter().map(ToString::to_string).collect();
            let err = validate_resolved_ips_are_public("attacker.test", &ips, &prefixes)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("non-global address 10.0.0.1"),
                "are_public with prefixes {order:?} produced unexpected error: {err}"
            );
            assert!(
                err.contains(OVERLAP_SPECIFIC_96),
                "error must name the prefix whose translation was denied: {err}"
            );
        }
    }

    #[test]
    fn overlapping_prefixes_reject_when_the_broad_translation_is_denied() {
        // The mirror of the reviewer's example: here the *specific* /96
        // decodes 2001:67c:a9fe:a9fe:1234:5678:5db8:d822 to the global
        // 93.184.216.34 while the broad /32 decodes it to 169.254.169.254.
        // Rejection must not depend on which of the two overlapping prefixes
        // carries the denied translation, nor on the order they are supplied.
        let ips = [ip("2001:67c:a9fe:a9fe:1234:5678:5db8:d822")];
        assert!(!is_cloud_metadata_ip(ips[0]));

        for prefixes in both_orders(OVERLAP_BROAD_32, OVERLAP_MIRROR_SPECIFIC_96) {
            let order: Vec<String> = prefixes.iter().map(ToString::to_string).collect();

            for (validator, err) in [
                (
                    "are_public",
                    validate_resolved_ips_are_public("attacker.test", &ips, &prefixes),
                ),
                (
                    "exclude_metadata",
                    validate_resolved_ips_exclude_metadata("attacker.test", &ips, &prefixes),
                ),
            ] {
                let err = err.unwrap_err().to_string();
                assert!(
                    err.contains("cloud metadata address 169.254.169.254"),
                    "{validator} with prefixes {order:?} produced unexpected error: {err}"
                );
                assert!(
                    err.contains(OVERLAP_BROAD_32),
                    "error must name the prefix whose translation was denied: {err}"
                );
            }
        }
    }

    #[test]
    fn overlapping_prefixes_accept_when_every_translation_is_global() {
        // Evaluating every matching prefix must not turn into blanket refusal
        // of overlapping configurations: 2001:67c:5db8:d822:1234:5678:5db8:d822
        // decodes to 93.184.216.34 under both prefixes, so both validators
        // accept it.
        let ips = [ip("2001:67c:5db8:d822:1234:5678:5db8:d822")];

        for prefixes in both_orders(OVERLAP_BROAD_32, OVERLAP_SPECIFIC_96) {
            let order: Vec<String> = prefixes.iter().map(ToString::to_string).collect();
            assert!(
                validate_resolved_ips_are_public("cdn.test", &ips, &prefixes).is_ok(),
                "are_public must accept a doubly-global translation, prefixes {order:?}"
            );
            assert!(
                validate_resolved_ips_exclude_metadata("cdn.test", &ips, &prefixes).is_ok(),
                "exclude_metadata must accept a doubly-global translation, prefixes {order:?}"
            );
        }
    }

    #[test]
    fn configured_prefix_does_not_relax_the_well_known_prefix_or_raw_classification() {
        let prefixes = nat64(GLOBAL_NAT64_96);
        for address in ["64:ff9b::10.0.0.1", "::ffff:169.254.169.254", "2001:db8::1"] {
            let ips = [ip(address)];
            assert!(
                validate_resolved_ips_are_public("example.test", &ips, &prefixes).is_err(),
                "{address} must stay blocked with a network-specific prefix configured"
            );
        }
    }

    // ── pinned destinations ───────────────────────────────────────

    fn sock(address: &str, port: u16) -> std::net::SocketAddr {
        std::net::SocketAddr::new(ip(address), port)
    }

    fn pin(
        host: &str,
        addresses: &[std::net::SocketAddr],
        access: PrivateNetworkAccess,
    ) -> Result<ResolvedDestination, NetworkGuardError> {
        ResolvedDestination::new(host, 443, addresses.iter().copied(), access, &[])
    }

    #[test]
    fn destination_retains_exactly_the_validated_addresses() {
        let destination = pin(
            "api.example.com",
            &[sock("1.1.1.1", 443), sock("8.8.8.8", 443)],
            PrivateNetworkAccess::Deny,
        )
        .unwrap();
        assert_eq!(destination.host(), "api.example.com");
        assert_eq!(destination.port(), 443);
        assert_eq!(
            destination.addresses(),
            [sock("1.1.1.1", 443), sock("8.8.8.8", 443)],
            "the pin must be the answer set that was validated, in resolver order"
        );
    }

    /// The mixed-class rejection. Under `Allow` neither validator objects to
    /// this set, so this check is the only thing standing between the caller
    /// and a connection whose trust zone is decided by resolver ordering.
    #[test]
    fn destination_rejects_a_mixed_public_private_answer_set() {
        assert_eq!(
            pin(
                "rebind.example.com",
                &[sock("1.1.1.1", 443), sock("10.0.0.5", 443)],
                PrivateNetworkAccess::Allow,
            ),
            Err(NetworkGuardError::MixedAddressClasses)
        );
        // Either ordering, so the check cannot pass by inspecting the public
        // address first.
        assert_eq!(
            pin(
                "rebind.example.com",
                &[sock("10.0.0.5", 443), sock("1.1.1.1", 443)],
                PrivateNetworkAccess::Allow,
            ),
            Err(NetworkGuardError::MixedAddressClasses)
        );
        // An all-private set under the same opt-in is fine.
        assert!(
            pin(
                "gitea.internal.example",
                &[sock("10.0.0.5", 443), sock("10.0.0.6", 443)],
                PrivateNetworkAccess::Allow,
            )
            .is_ok()
        );
    }

    /// A NAT64-translated private destination puts the answer in the private
    /// zone even though the literal IPv6 address is globally routable, so a
    /// mixed set must still be caught with a translator configured.
    #[test]
    fn mixed_class_detection_follows_configured_nat64_translation() {
        let prefixes = nat64(GLOBAL_NAT64_96);
        let translated = "2001:67c:2b0:db32:0:1:a00:5"; // -> 10.0.0.5
        assert_eq!(
            ResolvedDestination::new(
                "rebind.example.com",
                443,
                [sock("1.1.1.1", 443), sock(translated, 443)],
                PrivateNetworkAccess::Allow,
                &prefixes,
            ),
            Err(NetworkGuardError::MixedAddressClasses)
        );
        // Without the translator declared, that address is simply global, and
        // the set is uniformly public.
        assert!(
            ResolvedDestination::new(
                "rebind.example.com",
                443,
                [sock("1.1.1.1", 443), sock(translated, 443)],
                PrivateNetworkAccess::Allow,
                &[],
            )
            .is_ok()
        );
    }

    /// One IPv6 answer whose overlapping declared translations land in two
    /// trust zones is itself a mixed answer: a broad prefix decodes it to a
    /// global destination while a nested prefix decodes it to a private one,
    /// and which translation carries the connection is the network's choice.
    /// Collapsing the interpretations to a single class would accept it as
    /// simply private under the opt-in, so the class set must be preserved
    /// per address, in either declaration order.
    #[test]
    fn overlapping_translations_of_one_answer_are_a_mixed_class_set() {
        let address = "2001:67c:5db8:d822:1234:5678:a00:1"; // /32 -> 93.184.216.34, /96 -> 10.0.0.1
        for order in [
            ["2001:67c::/32", "2001:67c:5db8:d822:1234:5678::/96"],
            ["2001:67c:5db8:d822:1234:5678::/96", "2001:67c::/32"],
        ] {
            let prefixes = nat64_in_order(&order);
            assert_eq!(
                ResolvedDestination::new(
                    "rebind.example.com",
                    443,
                    [sock(address, 443)],
                    PrivateNetworkAccess::Allow,
                    &prefixes,
                ),
                Err(NetworkGuardError::MixedAddressClasses),
                "declaration order {order:?} must not change the verdict"
            );
        }
        // With only the broad prefix declared, both interpretations (raw and
        // the /32 translation) are global, and the answer is uniformly public.
        assert!(
            ResolvedDestination::new(
                "rebind.example.com",
                443,
                [sock(address, 443)],
                PrivateNetworkAccess::Allow,
                &nat64_in_order(&["2001:67c::/32"]),
            )
            .is_ok()
        );
    }

    /// Metadata stays refused under the private opt-in, which is the mode where
    /// no other address-class check runs.
    #[test]
    fn destination_refuses_metadata_even_with_private_access_granted() {
        let error = pin(
            "metadata.internal.example",
            &[sock("169.254.169.254", 443)],
            PrivateNetworkAccess::Allow,
        )
        .unwrap_err();
        assert!(
            matches!(error, NetworkGuardError::CloudMetadata { .. }),
            "unexpected error: {error}"
        );

        let prefixes = nat64(DOC_NAT64_96);
        let translated = "2001:db8:122:344::a9fe:a9fe"; // -> 169.254.169.254
        let error = ResolvedDestination::new(
            "metadata.internal.example",
            443,
            [sock(translated, 443)],
            PrivateNetworkAccess::Allow,
            &prefixes,
        )
        .unwrap_err();
        assert!(
            matches!(error, NetworkGuardError::CloudMetadata { .. }),
            "a NAT64-translated metadata address must be refused too, got: {error}"
        );
    }

    #[test]
    fn destination_denies_private_answers_without_the_opt_in() {
        let error = pin(
            "gitea.example.com",
            &[sock("10.0.0.5", 443)],
            PrivateNetworkAccess::Deny,
        )
        .unwrap_err();
        assert!(
            matches!(error, NetworkGuardError::PrivateNetworkDenied { .. }),
            "unexpected error: {error}"
        );
        assert_eq!(
            pin(
                "localhost",
                &[sock("127.0.0.1", 443)],
                PrivateNetworkAccess::Deny
            ),
            Err(NetworkGuardError::PrivateHostDenied(
                "localhost".to_string()
            )),
            "a syntactically local name is refused before resolution is even consulted"
        );
    }

    /// The verdict must be the validators', not a second opinion: for every
    /// address class, `Deny` mode and `validate_resolved_ips_are_public` agree.
    #[test]
    fn destination_verdict_agrees_with_the_shared_validator() {
        let prefixes = nat64(GLOBAL_NAT64_96);
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "10.0.0.5",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "2606:4700:4700::1111",
            "64:ff9b::10.0.0.1",
            "::ffff:169.254.169.254",
            "2001:67c:2b0:db32:0:1:a00:5",
            "2001:67c:2b0:db32:0:1:101:101",
        ] {
            let validator =
                validate_resolved_ips_are_public("agree.test", &[ip(address)], &prefixes);
            let destination = ResolvedDestination::new(
                "agree.test",
                443,
                [sock(address, 443)],
                PrivateNetworkAccess::Deny,
                &prefixes,
            );
            assert_eq!(
                validator.is_ok(),
                destination.is_ok(),
                "{address}: validator and pinned destination must agree"
            );
        }
    }

    #[test]
    fn destination_rejects_structurally_malformed_answers() {
        assert_eq!(
            ResolvedDestination::new(
                "api.example.com",
                0,
                [sock("1.1.1.1", 0)],
                PrivateNetworkAccess::Deny,
                &[],
            ),
            Err(NetworkGuardError::InvalidPort)
        );
        assert!(matches!(
            pin("api.example.com", &[], PrivateNetworkAccess::Deny),
            Err(NetworkGuardError::NoAddresses { .. })
        ));
        assert!(matches!(
            pin(
                "api.example.com",
                &[sock("1.1.1.1", 80)],
                PrivateNetworkAccess::Deny
            ),
            Err(NetworkGuardError::PortMismatch { .. })
        ));
        assert!(matches!(
            pin(
                "1.1.1.1",
                &[sock("8.8.8.8", 443)],
                PrivateNetworkAccess::Deny
            ),
            Err(NetworkGuardError::LiteralMismatch { .. })
        ));
        assert!(matches!(
            pin(
                "bad..example",
                &[sock("1.1.1.1", 443)],
                PrivateNetworkAccess::Deny
            ),
            Err(NetworkGuardError::InvalidHost(_))
        ));
    }

    #[test]
    fn destination_deduplicates_without_reordering() {
        let destination = pin(
            "api.example.com",
            &[
                sock("8.8.8.8", 443),
                sock("1.1.1.1", 443),
                sock("8.8.8.8", 443),
            ],
            PrivateNetworkAccess::Deny,
        )
        .unwrap();
        assert_eq!(
            destination.addresses(),
            [sock("8.8.8.8", 443), sock("1.1.1.1", 443)]
        );
    }

    #[test]
    fn request_host_normalization_is_strict_and_canonical() {
        assert_eq!(
            normalize_host("API.Example.COM.").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            normalize_host("[2001:4860:4860::8888]").unwrap(),
            "2001:4860:4860::8888"
        );
        for invalid in [
            "",
            " api.example",
            "bad..example",
            "[127.0.0.1]",
            "https://api.example.com",
            "api.example.com:8443",
            "user@api.example.com",
            "api.example.com/path",
        ] {
            assert!(
                normalize_host(invalid).is_err(),
                "{invalid:?} must not normalize to a request host"
            );
        }
    }
}
