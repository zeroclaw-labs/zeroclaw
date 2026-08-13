//! Network-safety primitives shared across crates that must reject SSRF and
//! local/private targets. Lives in `zeroclaw-infra` so both the tool layer
//! (`zeroclaw-tools` domain guard) and the plugin host (`zeroclaw-plugins`
//! `zc_http_request`) read one implementation without a tool-to-plugin

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

/// RFC 6052 well-known NAT64/DNS64 prefix `64:ff9b::/96`. DNS64 embeds the
/// IPv4 answer in the low 32 bits of the synthesized address, so e.g.
/// `64:ff9b::a9fe:a9fe` embeds `169.254.169.254` and `64:ff9b::a00:1`
/// embeds `10.0.0.1`. A hostname whose DNS64 answer is one of these forms
/// reaches the embedded IPv4 through the translator, so SSRF gates must
/// classify the embedded address — not treat the prefix as globally routable.
#[must_use]
pub fn nat64_embedded_ipv4(v6: std::net::Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let octets = v6.octets();
    let well_known_prefix = [
        0x00, 0x64, 0xff, 0x9b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    if octets[..12] == well_known_prefix {
        Some(std::net::Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ))
    } else {
        None
    }
}

/// A network-specific RFC 6052 NAT64/DNS64 translation prefix declared by the
/// operator. The well-known `64:ff9b::/96` prefix is built into
/// [`nat64_embedded_ipv4`] and needs no declaration; any other prefix a
/// deployment's DNS64 synthesizes under must be declared here, because a
/// network-specific prefix is chosen by the operator and cannot be detected
/// from an address alone (RFC 8215 §5 explicitly forbids assuming where an
/// embedded IPv4 sits inside a local-use prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nat64Prefix {
    pub prefix: std::net::Ipv6Addr,
    /// RFC 6052 §2.2 prefix length: one of 32, 40, 48, 56, 64, 96.
    pub len: u8,
}

/// Zero the host bits of `addr` below `len`, keeping the top `len` network
/// bits. A [`Nat64Prefix`] is kept in this canonical form so equivalent CIDRs
/// (`2606:4700:4700::/48` and `2606:4700:4700::1/48`, which describe the same
/// translation range) compare equal — otherwise the overlap rule for
/// equal-length declarations could be bypassed with non-canonical host bits.
#[must_use]
fn mask_to_prefix_bits(addr: std::net::Ipv6Addr, len: u8) -> std::net::Ipv6Addr {
    let bits = u128::from(addr) & (u128::MAX << (128 - u32::from(len)));
    std::net::Ipv6Addr::from(bits)
}

/// Parse an IPv6 CIDR (`"2001:db8:64::/96"`) into a [`Nat64Prefix`]. Accepts
/// only the RFC 6052 §2.2 prefix lengths (32, 40, 48, 56, 64, 96); a non-IPv6
/// address or any other length returns `None` so callers can fail closed on a
/// malformed entry. The parsed address is canonicalized to its top-`len` bits,
/// so a declared `2606:4700:4700::1/48` is stored as `2606:4700:4700::/48`.
#[must_use]
pub fn parse_nat64_prefix(cidr: &str) -> Option<Nat64Prefix> {
    let (addr, len) = cidr.split_once('/')?;
    let len: u8 = len.parse().ok()?;
    let prefix = addr.parse::<std::net::Ipv6Addr>().ok()?;
    match len {
        32 | 40 | 48 | 56 | 64 | 96 => Some(Nat64Prefix {
            prefix: mask_to_prefix_bits(prefix, len),
            len,
        }),
        _ => None,
    }
}

/// True when `v6` lies inside the declared RFC 6052 translation range — its
/// top `len` bits match `prefix`. Used to fail closed at a security gate when
/// extraction is impossible (e.g. a nonzero "u" octet) instead of treating an
/// unextractable in-range address as ordinary public IPv6.
#[must_use]
pub fn is_under_nat64_prefix(v6: std::net::Ipv6Addr, p: &Nat64Prefix) -> bool {
    let addr = u128::from(v6);
    let prefix_bits = u128::from(p.prefix);
    // The top `len` bits must equal the declared prefix.
    addr >> (128 - u32::from(p.len)) == prefix_bits >> (128 - u32::from(p.len))
}

/// True when `v6` lies inside any of the declared translation prefixes.
#[must_use]
pub fn is_under_any_nat64_prefix(v6: std::net::Ipv6Addr, declared: &[Nat64Prefix]) -> bool {
    declared.iter().any(|p| is_under_nat64_prefix(v6, p))
}

/// Extract the IPv4 address embedded in `v6` under a declared network-specific
/// prefix per RFC 6052 §2.2. The 32-bit IPv4 sits immediately after the
/// prefix; for prefix lengths where it would straddle bit 64 the zero "u"
/// octet is inserted at bits 64–71. Returns `None` unless the top `len` bits
/// match the declared prefix and the "u" octet is zero.
///
/// Reserved suffix bits beyond the embedded IPv4 are deliberately NOT required
/// to be zero. RFC 6052 §2.2 says an address translator that receives nonzero
/// suffix bits SHOULD ignore their value and proceed as if they were zero, so
/// a compliant network-specific translator routes a nonzero-suffix address to
/// its embedded IPv4. Requiring zero suffix bits here would let a nonzero-
/// suffix private/metadata embedding fall through to the ordinary-public-IPv6
/// path at the SSRF gate. Callers that cannot accept an unextractable in-range
/// address should combine this with [`is_under_any_nat64_prefix`] to fail
/// closed.
#[must_use]
pub fn nat64_embedded_ipv4_under_prefix(
    v6: std::net::Ipv6Addr,
    prefix: std::net::Ipv6Addr,
    len: u8,
) -> Option<std::net::Ipv4Addr> {
    if !matches!(len, 32 | 40 | 48 | 56 | 64 | 96) {
        return None;
    }
    if !is_under_nat64_prefix(v6, &Nat64Prefix { prefix, len }) {
        return None;
    }
    let addr = u128::from(v6);
    // Bit positions run MSB-first; `slice(lo, hi)` extracts bits [lo, hi).
    let slice = |lo: u32, hi: u32| -> u128 { (addr >> (128 - hi)) & ((1u128 << (hi - lo)) - 1) };
    // RFC 6052 §2.2: the zero "u" octet occupies bits 64–71 for every prefix
    // length below 96 (it is absent at /96, where the IPv4 fills the tail).
    if len < 96 && slice(64, 72) != 0 {
        return None;
    }
    let v4 = match len {
        32 => slice(32, 64),
        40 => (slice(40, 64) << 8) | slice(72, 80),
        48 => (slice(48, 64) << 16) | slice(72, 88),
        56 => (slice(56, 64) << 24) | slice(72, 96),
        64 => slice(72, 104),
        96 => slice(96, 128),
        _ => unreachable!(),
    };
    Some(std::net::Ipv4Addr::from(u32::try_from(v4).ok()?))
}

/// The IPv4 embedded in `v6` under any of the operator-declared prefixes, or
/// `None` when it is not a clean IPv4-embedded form of any declared prefix.
#[must_use]
pub fn nat64_embedded_ipv4_under_any(
    v6: std::net::Ipv6Addr,
    declared: &[Nat64Prefix],
) -> Option<std::net::Ipv4Addr> {
    declared
        .iter()
        .find_map(|p| nat64_embedded_ipv4_under_prefix(v6, p.prefix, p.len))
}

/// True when two declared network-specific NAT64 prefixes overlap — one is
/// contained by the other, or they are identical. Overlapping declarations
/// make embedded-IPv4 extraction order-dependent: a single IPv6 address inside
/// both prefixes decodes to a *different* IPv4 under each length (the RFC 6052
/// layout shifts the 32-bit IPv4 position as the prefix grows), so the SSRF
/// gate would classify the same resolved address as public under one ordering
/// and non-global/metadata under the other. Security decision must not depend
/// on configuration order, so callers reject overlapping sets as invalid
/// (fail closed) before dispatch.
#[must_use]
pub fn nat64_prefixes_overlap(a: &Nat64Prefix, b: &Nat64Prefix) -> bool {
    if a.len == b.len {
        // Equal-length declarations overlap when their top-`len` network bits
        // match — compare the masked identity, not the raw `Ipv6Addr`, so an
        // equivalent alias with nonzero host bits (`2606:4700:4700::1/48` vs
        // `2606:4700:4700::/48`) is still rejected even if it was constructed
        // directly rather than via [`parse_nat64_prefix`].
        return is_under_nat64_prefix(b.prefix, a);
    }
    // The shorter prefix (larger network) contains the longer one exactly when
    // the longer's address lies inside the shorter's range.
    let (shorter, longer) = if a.len < b.len { (a, b) } else { (b, a) };
    is_under_nat64_prefix(longer.prefix, shorter)
}

/// True when an IPv6 address is not globally routable (loopback, ULA,
/// link-local, documentation, multicast, an IPv4-mapped non-global v4, or an
/// RFC 6052 NAT64/DNS64 form embedding a non-global v4). The NAT64 branch
/// matters for SSRF: a DNS64-synthesized address is translated to its
/// embedded IPv4 on the wire, so `64:ff9b::a00:1` must be rejected just like
/// the `10.0.0.1` it reaches.
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
        || nat64_embedded_ipv4(v6).is_some_and(is_non_global_v4)
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
    fn nat64_v6_follows_embedded_v4_classification() {
        // RFC 6052 well-known prefix 64:ff9b::/96 — the synthesized address is
        // translated to its embedded IPv4 on the wire, so classification must
        // follow the embedded address.
        assert!(
            is_non_global_v6("64:ff9b::a00:1".parse::<Ipv6Addr>().unwrap()),
            "64:ff9b::a00:1 embeds 10.0.0.1 (RFC 1918) and must be non-global"
        );
        assert!(
            is_non_global_v6("64:ff9b::7f00:1".parse::<Ipv6Addr>().unwrap()),
            "64:ff9b::7f00:1 embeds 127.0.0.1 (loopback) and must be non-global"
        );
        assert!(
            is_non_global_v6("64:ff9b::a9fe:a9fe".parse::<Ipv6Addr>().unwrap()),
            "64:ff9b::a9fe:a9fe embeds 169.254.169.254 (link-local) and must be non-global"
        );
        // A NAT64 form embedding a genuinely public IPv4 must stay allowed —
        // it reaches the same public endpoint the IPv4 form would.
        assert!(
            !is_non_global_v6("64:ff9b::808:808".parse::<Ipv6Addr>().unwrap()),
            "64:ff9b::808:808 embeds 8.8.8.8 (public) and must remain global"
        );
        // Non-64:ff9b prefixes are not NAT64 forms: the same low-32-bits
        // pattern under a globally routable prefix (2001:4860::/32 is Google's
        // global range) must not be classified through the NAT64 branch.
        assert!(!is_non_global_v6(
            "2001:4860::a00:1".parse::<Ipv6Addr>().unwrap()
        ));
    }

    #[test]
    fn nat64_embedded_ipv4_extracts_well_known_prefix() {
        assert_eq!(
            nat64_embedded_ipv4("64:ff9b::a9fe:a9fe".parse::<Ipv6Addr>().unwrap()),
            Some(Ipv4Addr::new(169, 254, 169, 254)),
        );
        assert_eq!(
            nat64_embedded_ipv4("64:ff9b::a00:1".parse::<Ipv6Addr>().unwrap()),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        assert_eq!(
            nat64_embedded_ipv4("2001:db8::1".parse::<Ipv6Addr>().unwrap()),
            None
        );
        assert_eq!(
            nat64_embedded_ipv4("64:ff9b::1:2:3:4".parse::<Ipv6Addr>().unwrap()),
            None,
            "bits above the embedded IPv4 must not be accepted"
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

    #[test]
    fn parse_nat64_prefix_accepts_rfc6052_lengths_only() {
        // The declared address is canonicalized to its top-`len` bits: for
        // `2001:db8:64::` the /32 and /40 forms mask away the nonzero `0064`
        // host group, while /48+ keep it. The canonical identity is what the
        // overlap rule and the extraction path compare against.
        for (cidr, expected_len, expected_prefix) in [
            ("2001:db8:64::/32", 32u8, "2001:db8::"),
            ("2001:db8:64::/40", 40, "2001:db8::"),
            ("2001:db8:64::/48", 48, "2001:db8:64::"),
            ("2001:db8:64::/56", 56, "2001:db8:64::"),
            ("2001:db8:64::/64", 64, "2001:db8:64::"),
            ("64:ff9b:1::/96", 96, "64:ff9b:1::"),
        ] {
            let p = parse_nat64_prefix(cidr).unwrap();
            assert_eq!(p.len, expected_len, "{cidr}");
            assert_eq!(
                p.prefix,
                expected_prefix.parse::<std::net::Ipv6Addr>().unwrap(),
                "{cidr}"
            );
        }
        // Non-RFC 6052 lengths must be rejected so config can fail closed.
        for bad in [
            "2001:db8::/24",
            "2001:db8::/0",
            "2001:db8::/97",
            "2001:db8::/128",
        ] {
            assert!(parse_nat64_prefix(bad).is_none(), "{bad}");
        }
        // Non-CIDR, IPv4, or unparsable input is rejected.
        assert!(parse_nat64_prefix("2001:db8:64::").is_none());
        assert!(parse_nat64_prefix("192.0.2.1/24").is_none());
        assert!(parse_nat64_prefix("not-a-cidr").is_none());
    }

    #[test]
    fn nat64_embedded_ipv4_under_prefix_matches_rfc6052_examples() {
        // RFC 6052 §2.2 Table 1: the /48 prefix 2001:db8:122:: embeds
        // 192.0.2.33 as 2001:db8:122:c000:2:2100:: (v4 split around the u octet).
        assert_eq!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8:122:c000:2:2100::".parse().unwrap(),
                "2001:db8:122::".parse().unwrap(),
                48,
            ),
            Some(Ipv4Addr::new(192, 0, 2, 33)),
        );
        // /96: the v4 fills the tail.
        assert_eq!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8:64::a00:1".parse().unwrap(),
                "2001:db8:64::".parse().unwrap(),
                96,
            ),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
    }

    #[test]
    fn nat64_embedded_ipv4_under_prefix_roundtrips_every_length() {
        let prefix: Ipv6Addr = "2001:db8:64::".parse().unwrap();
        // Vector addresses generated per RFC 6052 §2.2 for v4 = 10.0.0.1.
        for (len, addr) in [
            (32u8, "2001:db8:a00:1::"),
            (40u8, "2001:db8:a:0:1::"),
            (48u8, "2001:db8:64:a00:0:100::"),
            (56u8, "2001:db8:64:a:0:1::"),
            (64u8, "2001:db8:64:0:a:0:100:0"),
            (96u8, "2001:db8:64::a00:1"),
        ] {
            assert_eq!(
                nat64_embedded_ipv4_under_prefix(addr.parse().unwrap(), prefix, len),
                Some(Ipv4Addr::new(10, 0, 0, 1)),
                "len={len} addr={addr}"
            );
        }
    }

    #[test]
    fn nat64_embedded_ipv4_under_prefix_rejects_non_matches() {
        // Prefix bits must match the declared prefix exactly.
        assert!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8:65::a00:1".parse().unwrap(),
                "2001:db8:64::".parse().unwrap(),
                96,
            )
            .is_none()
        );
        // A non-zero "u" octet (byte 8 for /48) must be rejected.
        assert!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8:64:a00:100:100::".parse().unwrap(),
                "2001:db8:64::".parse().unwrap(),
                48,
            )
            .is_none()
        );
        // Non-zero suffix bits are IGNORED per RFC 6052 §2.2 (a translator
        // receives them and proceeds as if they were zero): byte 11 is nonzero
        // for /48 here, yet the embedded 10.0.0.1 is still extracted.
        assert_eq!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8:64:a00:0:101::".parse().unwrap(),
                "2001:db8:64::".parse().unwrap(),
                48,
            ),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        // Non-zero suffix for /64 (byte 13) is likewise ignored.
        assert_eq!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8:64:0:a:0:101:0".parse().unwrap(),
                "2001:db8:64::".parse().unwrap(),
                64,
            ),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        // Unsupported prefix lengths are not classified.
        assert!(
            nat64_embedded_ipv4_under_prefix(
                "2001:db8::1".parse().unwrap(),
                "2001:db8::".parse().unwrap(),
                24,
            )
            .is_none()
        );
    }

    #[test]
    fn nat64_embedded_ipv4_under_any_only_matches_declared_prefixes() {
        let declared = [Nat64Prefix {
            prefix: "2001:db8:64::".parse().unwrap(),
            len: 96,
        }];
        assert_eq!(
            nat64_embedded_ipv4_under_any("2001:db8:64::a00:1".parse().unwrap(), &declared),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        // The same tail under an undeclared prefix is not a NAT64 form.
        assert_eq!(
            nat64_embedded_ipv4_under_any("2001:db8:65::a00:1".parse().unwrap(), &declared),
            None,
        );
        // No declared prefixes -> nothing is classified.
        assert_eq!(
            nat64_embedded_ipv4_under_any("2001:db8:64::a00:1".parse().unwrap(), &[]),
            None,
        );
    }

    #[test]
    fn rfc8215_local_use_prefix_embeddings_classify_by_declared_length() {
        // 64:ff9b:1::/48 is reserved for local-use translation (RFC 8215); when
        // an operator declares it, the RFC 6052 §2.2 /48 layout is extracted so
        // a DNS64-synthesized private/metadata target is not treated as global.
        let declared = [Nat64Prefix {
            prefix: "64:ff9b:1::".parse().unwrap(),
            len: 48,
        }];
        assert_eq!(
            nat64_embedded_ipv4_under_any("64:ff9b:1:a00:0:100::".parse().unwrap(), &declared),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        assert_eq!(
            nat64_embedded_ipv4_under_any("64:ff9b:1:a9fe:a9:fe00::".parse().unwrap(), &declared),
            Some(Ipv4Addr::new(169, 254, 169, 254)),
        );
        assert_eq!(
            nat64_embedded_ipv4_under_any("64:ff9b:1:7f00:0:100::".parse().unwrap(), &declared),
            Some(Ipv4Addr::new(127, 0, 0, 1)),
        );
    }

    #[test]
    fn nat64_prefixes_overlap_detects_containment_equality_and_disjoint_sets() {
        let p32 = Nat64Prefix {
            prefix: "2606:4700::".parse().unwrap(),
            len: 32,
        };
        let p48 = Nat64Prefix {
            prefix: "2606:4700:4700::".parse().unwrap(),
            len: 48,
        };
        let disjoint = Nat64Prefix {
            prefix: "2001:db8:64::".parse().unwrap(),
            len: 96,
        };
        // /48 sits entirely inside /32: overlapping.
        assert!(nat64_prefixes_overlap(&p32, &p48));
        assert!(nat64_prefixes_overlap(&p48, &p32));
        // Identical declarations overlap.
        assert!(nat64_prefixes_overlap(&p48, &p48));
        // Disjoint prefixes do not overlap.
        assert!(!nat64_prefixes_overlap(&p32, &disjoint));
        assert!(!nat64_prefixes_overlap(&disjoint, &p48));
    }

    #[test]
    fn nat64_prefixes_overlap_rejects_equivalent_cidr_aliases() {
        // `2606:4700:4700::1/48` and `2606:4700:4700::/48` describe the SAME
        // translation range (only the top 48 bits matter), so the equal-length
        // branch must treat them as overlapping in BOTH declaration orders —
        // a non-canonical alias must not bypass the overlap rule.
        let canonical = Nat64Prefix {
            prefix: "2606:4700:4700::".parse().unwrap(),
            len: 48,
        };
        let alias = Nat64Prefix {
            prefix: "2606:4700:4700::1".parse().unwrap(),
            len: 48,
        };
        assert!(nat64_prefixes_overlap(&canonical, &alias));
        assert!(nat64_prefixes_overlap(&alias, &canonical));
        // A genuinely different /48 with a nonzero host bit is NOT the same
        // range and must not be reported as overlapping.
        let different = Nat64Prefix {
            prefix: "2606:4700:4701::2".parse().unwrap(),
            len: 48,
        };
        assert!(!nat64_prefixes_overlap(&canonical, &different));
        assert!(!nat64_prefixes_overlap(&different, &canonical));
    }

    #[test]
    fn parse_nat64_prefix_canonicalizes_host_bits() {
        // Parsing must mask host bits below the prefix length so equivalent
        // aliases normalize to one identity (`2606:4700:4700::/48`).
        let p = parse_nat64_prefix("2606:4700:4700::1/48").unwrap();
        assert_eq!(
            p,
            Nat64Prefix {
                prefix: "2606:4700:4700::".parse().unwrap(),
                len: 48,
            }
        );
        assert_eq!(p, parse_nat64_prefix("2606:4700:4700::/48").unwrap());
    }

    #[test]
    fn nat64_embedded_ipv4_under_any_is_order_dependent_for_overlapping_prefixes() {
        // With overlapping declarations the SAME address decodes to different
        // IPv4s under each prefix length, flipping the SSRF classification
        // between public and non-global — the reason the gate rejects
        // overlapping sets as an invalid configuration.
        let addr: std::net::Ipv6Addr = "2606:4700:4700:a00:0:100::".parse().unwrap();
        let p32 = [Nat64Prefix {
            prefix: "2606:4700::".parse().unwrap(),
            len: 32,
        }];
        let p48 = [Nat64Prefix {
            prefix: "2606:4700:4700::".parse().unwrap(),
            len: 48,
        }];
        // Under /32 the embedded IPv4 is public (71.0.10.0)...
        assert_eq!(
            nat64_embedded_ipv4_under_any(addr, &p32),
            Some(Ipv4Addr::new(71, 0, 10, 0)),
        );
        // ...but under the overlapping /48 it is private (10.0.0.1). Declaration
        // order therefore decides whether the gate blocks the same address.
        assert_eq!(
            nat64_embedded_ipv4_under_any(addr, &p48),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
    }

    #[test]
    fn nat64_embedded_ipv4_ignores_nonzero_suffix_bits() {
        // RFC 6052 §2.2: an address translator that receives nonzero reserved
        // suffix bits SHOULD ignore their value and proceed as if they were
        // zero, so a compliant network-specific translator routes these to
        // their embedded IPv4. The extractor must classify them, not return
        // None and let the address fall through to ordinary-public-IPv6 at the
        // SSRF gate.
        let declared = [Nat64Prefix {
            prefix: "64:ff9b:1::".parse().unwrap(),
            len: 48,
        }];
        // 64:ff9b:1:a00:0:100:0:1 -> 10.0.0.1 (nonzero suffix byte 15).
        assert_eq!(
            nat64_embedded_ipv4_under_any("64:ff9b:1:a00:0:100:0:1".parse().unwrap(), &declared),
            Some(Ipv4Addr::new(10, 0, 0, 1)),
        );
        // 64:ff9b:1:a9fe:a9:fe00:0:1 -> 169.254.169.254 (metadata, nonzero
        // suffix byte 15).
        assert_eq!(
            nat64_embedded_ipv4_under_any("64:ff9b:1:a9fe:a9:fe00:0:1".parse().unwrap(), &declared),
            Some(Ipv4Addr::new(169, 254, 169, 254)),
        );
        // A nonzero "u" octet still rejects: the extractor cannot classify it,
        // and callers fail closed via is_under_any_nat64_prefix instead.
        assert!(
            nat64_embedded_ipv4_under_any("64:ff9b:1:a00:100:100::".parse().unwrap(), &declared)
                .is_none()
        );
    }

    #[test]
    fn is_under_any_nat64_prefix_matches_only_declared_ranges() {
        let declared = [Nat64Prefix {
            prefix: "64:ff9b:1::".parse().unwrap(),
            len: 48,
        }];
        assert!(is_under_any_nat64_prefix(
            "64:ff9b:1:a00:0:100:0:1".parse().unwrap(),
            &declared
        ));
        // The same tail under an undeclared prefix is outside the range.
        assert!(!is_under_any_nat64_prefix(
            "64:ff9b:2::1".parse().unwrap(),
            &declared
        ));
        assert!(!is_under_any_nat64_prefix(
            "64:ff9b::1".parse().unwrap(),
            &declared
        ));
        // No declared prefixes -> nothing is inside a range.
        assert!(!is_under_any_nat64_prefix(
            "64:ff9b:1::1".parse().unwrap(),
            &[]
        ));
    }
}
