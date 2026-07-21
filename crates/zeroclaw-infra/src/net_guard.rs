//! Network-safety primitives shared across crates that must reject SSRF and
//! local/private targets. Lives in `zeroclaw-infra` so tools and plugin
//! transports read one implementation without depending on each other.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};

const EC2_IMDS_V4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(169, 254, 169, 254);
const EC2_IMDS_V6: std::net::Ipv6Addr =
    std::net::Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// Whether an authorized destination may resolve to private/local addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivateNetworkAccess {
    /// Every resolved address must be globally routable.
    Deny,
    /// An all-private answer set is accepted, except cloud metadata addresses.
    Allow,
}

/// Why a host or its resolved address set is unsafe to dial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkGuardError {
    /// The host is empty, malformed, or not a canonical DNS name/IP literal.
    InvalidHost(String),
    /// Port zero is never a valid outbound destination.
    InvalidPort,
    /// DNS returned no addresses.
    NoAddresses { host: String, port: u16 },
    /// A resolver returned an address for a different port.
    PortMismatch { expected: u16, address: SocketAddr },
    /// A literal IP resolved to a different IP.
    LiteralMismatch {
        literal: IpAddr,
        address: SocketAddr,
    },
    /// The answer set contains a cloud metadata endpoint, which is never allowed.
    CloudMetadata(SocketAddr),
    /// Private/local resolution was not authorized for this host.
    PrivateNetworkDenied(SocketAddr),
    /// A syntactically local hostname was not explicitly authorized.
    PrivateHostDenied(String),
    /// A local hostname unexpectedly resolved only to public addresses.
    PrivateHostResolvedPublic(String),
    /// DNS returned both globally routable and private/local addresses.
    MixedAddressClasses,
}

impl fmt::Display for NetworkGuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
            Self::CloudMetadata(address) => {
                write!(f, "cloud metadata address {address} is never allowed")
            }
            Self::PrivateNetworkDenied(address) => write!(
                f,
                "private or non-global address {address} is not authorized"
            ),
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

/// A normalized host and the exact address set that passed network policy.
///
/// Callers must dial [`Self::addresses`] directly. Resolving [`Self::host`]
/// again would reopen the DNS-rebinding window this type closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDestination {
    host: String,
    port: u16,
    addresses: Vec<SocketAddr>,
}

impl ResolvedDestination {
    /// Validate one DNS result and retain the exact addresses to dial.
    ///
    /// Cloud metadata is always denied. Mixed public/private answer sets are
    /// denied even when private access is authorized, preventing resolver
    /// ordering or connection fallback from silently changing trust zones.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkGuardError`] for malformed input, empty/mismatched DNS
    /// answers, metadata endpoints, unauthorized private addresses, or mixed
    /// address classes.
    pub fn new(
        host: &str,
        port: u16,
        addresses: impl IntoIterator<Item = SocketAddr>,
        private_access: PrivateNetworkAccess,
    ) -> Result<Self, NetworkGuardError> {
        let host = normalize_host(host)?;
        if port == 0 {
            return Err(NetworkGuardError::InvalidPort);
        }

        let literal = host.parse::<IpAddr>().ok();
        let private_host = is_private_or_local_host(&host);
        if private_host && private_access == PrivateNetworkAccess::Deny {
            return Err(NetworkGuardError::PrivateHostDenied(host.clone()));
        }
        let mut unique = HashSet::new();
        let mut addresses = addresses
            .into_iter()
            .filter(|address| unique.insert(*address))
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(NetworkGuardError::NoAddresses { host, port });
        }

        let mut saw_public = false;
        let mut saw_private = false;
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
            if is_cloud_metadata_ip(address.ip()) {
                return Err(NetworkGuardError::CloudMetadata(*address));
            }
            if is_non_global_ip(address.ip()) {
                saw_private = true;
                if private_access == PrivateNetworkAccess::Deny {
                    return Err(NetworkGuardError::PrivateNetworkDenied(*address));
                }
            } else {
                saw_public = true;
            }
        }

        if saw_public && saw_private {
            return Err(NetworkGuardError::MixedAddressClasses);
        }
        if private_host && saw_public {
            return Err(NetworkGuardError::PrivateHostResolvedPublic(host.clone()));
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
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
}

/// Normalize a DNS host or IP literal for policy matching and SNI selection.
///
/// DNS names are ASCII-only; internationalized names must use their punycode
/// form. IPv6 brackets are accepted and removed.
///
/// # Errors
///
/// Returns [`NetworkGuardError::InvalidHost`] for malformed hosts.
pub fn normalize_host(host: &str) -> Result<String, NetworkGuardError> {
    zeroclaw_api::plugin_egress::normalize_outbound_host(host)
        .ok_or_else(|| NetworkGuardError::InvalidHost(host.to_string()))
}

/// True for cloud instance-metadata endpoints. These remain blocked even when
/// an operator explicitly authorizes private-network egress.
#[must_use]
pub fn is_cloud_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip == EC2_IMDS_V4,
        IpAddr::V6(ip) => ip == EC2_IMDS_V6,
    }
}

/// True when an address is not globally routable.
#[must_use]
pub fn is_non_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_non_global_v4(ip),
        IpAddr::V6(ip) => is_non_global_v6(ip),
    }
}

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
    a == 0 // Current network / unspecified source (0.0.0.0/8)
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b)) // RFC 6598 shared address space
        || a >= 240 // Reserved
        || (a == 192 && b == 0 && (c == 0 || c == 2)) // 192.0.0.0/24, 192.0.2.0/24
        || (a == 192 && b == 88 && c == 99) // Deprecated 6to4 relay anycast
        || (a == 198 && b == 51) // Documentation (198.51.100.0/24)
        || (a == 203 && b == 0) // Documentation (203.0.113.0/24)
        || (a == 198 && (18..=19).contains(&b)) // Benchmarking (198.18.0.0/15)
}

/// True when an IPv6 address is not globally routable (loopback, ULA,
/// link-local, documentation, multicast, or an IPv4-mapped non-global v4).
#[must_use]
pub fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_non_global_v4(v4);
    }
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xe000) != 0x2000 // Outside global-unicast 2000::/3
        || (segs[0] & 0xfe00) == 0xfc00 // Unique-local (fc00::/7)
        || (segs[0] & 0xffc0) == 0xfe80 // Link-local (fe80::/10)
        || (segs[0] & 0xffc0) == 0xfec0 // Deprecated site-local (fec0::/10)
        || (segs[0] == 0x0064 && segs[1] == 0xff9b) // IPv4 translation prefixes
        || (segs[0] == 0x2001 && segs[1] <= 0x01ff) // IETF assignments (2001::/23)
        || (segs[0] == 0x2001 && segs[1] == 0x0db8) // Documentation (2001:db8::/32)
        || segs[0] == 0x2002 // 6to4 can tunnel an otherwise-denied IPv4 target
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn addr(ip: &str, port: u16) -> SocketAddr {
        SocketAddr::new(ip.parse().expect("test IP must parse"), port)
    }

    #[test]
    fn normalize_host_canonicalizes_dns_and_ip_literals() {
        assert_eq!(
            normalize_host("API.Example.COM.").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            normalize_host("[2001:4860:4860::8888]").unwrap(),
            "2001:4860:4860::8888"
        );
        assert!(normalize_host(" user@example.com").is_err());
        assert!(normalize_host("bad..example").is_err());
        assert!(normalize_host("[127.0.0.1]").is_err());
    }

    #[test]
    fn resolved_destination_retains_only_checked_addresses() {
        let first = addr("1.1.1.1", 443);
        let second = addr("8.8.8.8", 443);
        let destination = ResolvedDestination::new(
            "Api.Example.com.",
            443,
            [first, second, first],
            PrivateNetworkAccess::Deny,
        )
        .unwrap();
        assert_eq!(destination.host(), "api.example.com");
        assert_eq!(destination.addresses(), &[first, second]);
    }

    #[test]
    fn resolved_destination_rejects_private_and_mixed_dns() {
        let private = addr("10.0.0.4", 443);
        let public = addr("1.1.1.1", 443);
        assert_eq!(
            ResolvedDestination::new(
                "internal.example",
                443,
                [private],
                PrivateNetworkAccess::Deny,
            )
            .unwrap_err(),
            NetworkGuardError::PrivateNetworkDenied(private)
        );
        assert_eq!(
            ResolvedDestination::new(
                "internal.example",
                443,
                [public, private],
                PrivateNetworkAccess::Allow,
            )
            .unwrap_err(),
            NetworkGuardError::MixedAddressClasses
        );
    }

    #[test]
    fn local_names_require_an_exception_and_must_stay_private() {
        let private = addr("127.0.0.1", 443);
        let public = addr("1.1.1.1", 443);
        assert_eq!(
            ResolvedDestination::new(
                "service.localhost",
                443,
                [private],
                PrivateNetworkAccess::Deny,
            )
            .unwrap_err(),
            NetworkGuardError::PrivateHostDenied("service.localhost".to_string())
        );
        assert_eq!(
            ResolvedDestination::new(
                "service.localhost",
                443,
                [public],
                PrivateNetworkAccess::Allow,
            )
            .unwrap_err(),
            NetworkGuardError::PrivateHostResolvedPublic("service.localhost".to_string())
        );
        assert!(
            ResolvedDestination::new(
                "service.localhost",
                443,
                [private],
                PrivateNetworkAccess::Allow,
            )
            .is_ok()
        );
    }

    #[test]
    fn resolved_destination_rejects_empty_and_mismatched_answers() {
        assert!(matches!(
            ResolvedDestination::new("api.example.com", 443, [], PrivateNetworkAccess::Deny,),
            Err(NetworkGuardError::NoAddresses { .. })
        ));
        let wrong_port = addr("1.1.1.1", 80);
        assert_eq!(
            ResolvedDestination::new(
                "api.example.com",
                443,
                [wrong_port],
                PrivateNetworkAccess::Deny,
            )
            .unwrap_err(),
            NetworkGuardError::PortMismatch {
                expected: 443,
                address: wrong_port,
            }
        );
    }

    #[test]
    fn private_exception_never_allows_cloud_metadata() {
        let metadata = addr("169.254.169.254", 80);
        assert_eq!(
            ResolvedDestination::new(
                "metadata.internal",
                80,
                [metadata],
                PrivateNetworkAccess::Allow,
            )
            .unwrap_err(),
            NetworkGuardError::CloudMetadata(metadata)
        );
        let metadata_v6 = addr("fd00:ec2::254", 80);
        assert_eq!(
            ResolvedDestination::new(
                "metadata.internal",
                80,
                [metadata_v6],
                PrivateNetworkAccess::Allow,
            )
            .unwrap_err(),
            NetworkGuardError::CloudMetadata(metadata_v6)
        );
    }

    #[test]
    fn literal_ip_cannot_rebind_to_another_address() {
        let unexpected = addr("1.0.0.1", 443);
        assert_eq!(
            ResolvedDestination::new("1.1.1.1", 443, [unexpected], PrivateNetworkAccess::Deny,)
                .unwrap_err(),
            NetworkGuardError::LiteralMismatch {
                literal: "1.1.1.1".parse().unwrap(),
                address: unexpected,
            }
        );
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
    fn cgnat_and_reserved_v4_blocked() {
        assert!(is_non_global_v4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_non_global_v4(Ipv4Addr::new(240, 0, 0, 1)));
    }

    #[test]
    fn special_ranges_cannot_bypass_private_address_checks() {
        for ip in [
            "0.1.2.3",
            "192.88.99.1",
            "64:ff9b::c0a8:1",
            "100::1",
            "2001:2::1",
            "2002:c0a8:1::1",
            "4000::1",
            "fec0::1",
        ] {
            assert!(
                is_non_global_ip(ip.parse().expect("test address must parse")),
                "{ip} must not be considered globally routable"
            );
        }
    }
}
