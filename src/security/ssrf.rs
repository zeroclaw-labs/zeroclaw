//! SSRF (Server-Side Request Forgery) protection.
//!
//! Provides a shared [`SsrfValidator`] used by all outbound HTTP tools
//! (`web_fetch`, `http_request`, `browser`) to enforce a consistent set of
//! network-access restrictions:
//!
//! - Private IP ranges (RFC 1918)
//! - Localhost and loopback
//! - Link-local (169.254.x.x, fe80::/10)
//! - Cloud metadata endpoints (169.254.169.254)
//! - Carrier-grade NAT and other reserved ranges
//! - Unicode homograph attacks (non-ASCII domains)
//! - DNS rebinding (double-resolve consistency check)
//! - Custom blocked CIDR ranges via [`SsrfValidator::add_blocked_range`]
//!
//! # Usage
//!
//! ```rust
//! use zeroclaw::security::SsrfValidator;
//!
//! let validator = SsrfValidator::default(); // block all private ranges
//! validator.validate_url("https://example.com/api").unwrap();
//! assert!(validator.validate_url("http://169.254.169.254/").is_err());
//! ```

use std::net::{IpAddr, ToSocketAddrs};
use std::str::FromStr;

use ipnetwork::IpNetwork;

/// SSRF validator with configurable blocked CIDR ranges.
///
/// Create with [`SsrfValidator::new`] or [`SsrfValidator::default`] (strict mode).
/// Add extra ranges with [`SsrfValidator::add_blocked_range`].
#[derive(Debug, Clone)]
pub struct SsrfValidator {
    blocked_ranges: Vec<IpNetwork>,
    /// When `true`, RFC-1918 private ranges are permitted (useful in trusted
    /// on-prem environments). Cloud metadata endpoints are still blocked.
    pub allow_private_ips: bool,
}

impl Default for SsrfValidator {
    fn default() -> Self {
        Self::new(false)
    }
}

impl SsrfValidator {
    /// Build a validator.
    ///
    /// `allow_private_ips = false` (the default) blocks all RFC-1918 ranges,
    /// localhost, and reserved ranges. `true` relaxes that but still blocks
    /// cloud-metadata endpoints.
    pub fn new(allow_private_ips: bool) -> Self {
        let blocked_ranges = if allow_private_ips {
            vec![
                // Always block cloud metadata even in trusted environments.
                IpNetwork::from_str("169.254.169.254/32").unwrap(), // AWS/GCP/Azure
            ]
        } else {
            vec![
                // RFC 1918 private ranges
                IpNetwork::from_str("10.0.0.0/8").unwrap(),
                IpNetwork::from_str("172.16.0.0/12").unwrap(),
                IpNetwork::from_str("192.168.0.0/16").unwrap(),
                // Loopback
                IpNetwork::from_str("127.0.0.0/8").unwrap(),
                IpNetwork::from_str("::1/128").unwrap(),
                // Link-local
                IpNetwork::from_str("169.254.0.0/16").unwrap(),
                IpNetwork::from_str("fe80::/10").unwrap(),
                // IPv4-mapped IPv6 loopback
                IpNetwork::from_str("::ffff:127.0.0.0/104").unwrap(),
                // Unspecified / "this" network
                IpNetwork::from_str("0.0.0.0/8").unwrap(),
                // Carrier-grade NAT (RFC 6598)
                IpNetwork::from_str("100.64.0.0/10").unwrap(),
                // IETF protocol assignments
                IpNetwork::from_str("192.0.0.0/24").unwrap(),
                // Documentation ranges (TEST-NET)
                IpNetwork::from_str("192.0.2.0/24").unwrap(),
                IpNetwork::from_str("198.51.100.0/24").unwrap(),
                IpNetwork::from_str("203.0.113.0/24").unwrap(),
                // Benchmarking
                IpNetwork::from_str("198.18.0.0/15").unwrap(),
                // Multicast / reserved
                IpNetwork::from_str("224.0.0.0/4").unwrap(),
                IpNetwork::from_str("240.0.0.0/4").unwrap(),
                // Broadcast
                IpNetwork::from_str("255.255.255.255/32").unwrap(),
                // Unique-local IPv6 (fc00::/7)
                IpNetwork::from_str("fc00::/7").unwrap(),
                // IPv6 documentation
                IpNetwork::from_str("2001:db8::/32").unwrap(),
            ]
        };

        Self {
            blocked_ranges,
            allow_private_ips,
        }
    }

    /// Add a custom blocked CIDR range (e.g. `"203.0.113.0/24"`).
    pub fn add_blocked_range(&mut self, cidr: &str) -> Result<(), String> {
        let network =
            IpNetwork::from_str(cidr).map_err(|e| format!("Invalid CIDR '{cidr}': {e}"))?;
        self.blocked_ranges.push(network);
        Ok(())
    }

    /// Validate a URL for SSRF vulnerabilities.
    ///
    /// Returns `Ok(())` when safe, `Err(description)` when blocked.
    pub fn validate_url(&self, url: &str) -> Result<(), String> {
        // 1. Only http / https allowed.
        let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
            ("https", r)
        } else if let Some(r) = url.strip_prefix("http://") {
            ("http", r)
        } else {
            let scheme_end = url.find("://").map(|i| &url[..i]).unwrap_or(url);
            return Err(format!(
                "Blocked URL scheme '{scheme_end}': only http:// and https:// are permitted"
            ));
        };

        // 2. Extract authority (host[:port]).
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if authority.is_empty() {
            return Err(format!("URL has no host: {url}"));
        }
        if authority.contains('@') {
            return Err("URL userinfo (@) is not permitted".into());
        }

        // 3. Split host and optional port.
        let (host, port_str) = if authority.starts_with('[') {
            // IPv6 literal: [::1]:port
            let end = authority
                .find(']')
                .ok_or_else(|| format!("Unclosed IPv6 bracket in URL: {url}"))?;
            let host = &authority[..=end];
            let port_part = &authority[end + 1..];
            let port_part = port_part.strip_prefix(':').unwrap_or("");
            (host, port_part)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, p),
                None => (authority, ""),
            }
        };

        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_lowercase();

        if host.is_empty() {
            return Err(format!("URL has no host: {url}"));
        }

        // 4. Reject non-ASCII domains (homograph attack prevention).
        if !host.is_ascii() {
            return Err(format!(
                "Blocked non-ASCII domain (potential homograph attack): {host}"
            ));
        }

        let port: u16 = if port_str.is_empty() {
            if scheme == "https" {
                443
            } else {
                80
            }
        } else {
            port_str
                .parse()
                .map_err(|_| format!("Invalid port in URL: {url}"))?
        };

        // 5. Resolve hostname → IPs and validate each.
        let socket_str = format!("{host}:{port}");

        let ips: Vec<IpAddr> = socket_str
            .to_socket_addrs()
            .map_err(|e| format!("Failed to resolve '{host}': {e}"))?
            .map(|sa| sa.ip())
            .collect();

        if ips.is_empty() {
            return Err(format!("'{host}' resolved to no addresses"));
        }

        for ip in &ips {
            self.check_ip(ip)?;
        }

        // 5. DNS rebinding protection: re-resolve and verify all IPs are still safe.
        //    (Legitimate round-robin CDNs may produce different sets — we warn but
        //    do not block on set mismatch; the safety check above already covers it.)
        let recheck: Vec<IpAddr> = socket_str
            .to_socket_addrs()
            .map_err(|e| format!("DNS recheck failed for '{host}': {e}"))?
            .map(|sa| sa.ip())
            .collect();

        for ip in &recheck {
            self.check_ip(ip)?;
        }

        if ips.len() != recheck.len() || !ips.iter().all(|ip| recheck.contains(ip)) {
            tracing::warn!(
                host,
                ?ips,
                ?recheck,
                "DNS resolution changed between SSRF checks (possible rebinding attempt or CDN)"
            );
        }

        Ok(())
    }

    /// Non-panicking convenience wrapper: returns `true` if the URL is blocked.
    pub fn is_blocked(&self, url: &str) -> bool {
        self.validate_url(url).is_err()
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn check_ip(&self, ip: &IpAddr) -> Result<(), String> {
        for range in &self.blocked_ranges {
            if range.contains(*ip) {
                return Err(format!("Blocked: {ip} matches restricted range {range}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict() -> SsrfValidator {
        SsrfValidator::new(false)
    }

    #[test]
    fn blocks_private_ipv4() {
        let v = strict();
        assert!(v.is_blocked("http://192.168.1.1/"));
        assert!(v.is_blocked("http://10.0.0.1/"));
        assert!(v.is_blocked("http://172.16.0.1/"));
    }

    #[test]
    fn blocks_localhost() {
        let v = strict();
        assert!(v.is_blocked("http://127.0.0.1/"));
        // "localhost" resolves to 127.0.0.1 in most environments; if DNS is
        // unavailable in CI the error is a resolution failure, not a security
        // pass — either way it does not return Ok.
        assert!(v.validate_url("http://localhost/").is_err());
    }

    #[test]
    fn blocks_cloud_metadata() {
        let v = strict();
        assert!(v.is_blocked("http://169.254.169.254/latest/meta-data/"));
    }

    #[test]
    fn blocks_metadata_with_allow_private() {
        // Even with allow_private_ips, metadata endpoint must be blocked.
        let v = SsrfValidator::new(true);
        assert!(v.is_blocked("http://169.254.169.254/"));
    }

    #[test]
    fn blocks_invalid_scheme() {
        let v = strict();
        assert!(v.is_blocked("file:///etc/passwd"));
        assert!(v.is_blocked("ftp://example.com/"));
        assert!(v.is_blocked("javascript:alert(1)"));
    }

    #[test]
    fn blocks_non_ascii_domain() {
        let v = strict();
        // Punycode-encoded look-alike domain: still ASCII, passes this check
        // (handled elsewhere). Raw non-ASCII must be blocked.
        let result = v.validate_url("http://еxample.com/"); // Cyrillic 'е'
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("homograph"));
    }

    #[test]
    fn custom_cidr_block() {
        let mut v = strict();
        v.add_blocked_range("8.8.8.0/24").unwrap();
        assert!(v.is_blocked("http://8.8.8.8/"));
    }

    #[test]
    fn invalid_cidr_returns_error() {
        let mut v = strict();
        assert!(v.add_blocked_range("not-a-cidr").is_err());
    }

    #[test]
    fn public_url_passes_validation_logic() {
        // We test the validation logic only; DNS may not resolve in CI.
        let v = strict();
        let result = v.validate_url("https://example.com/");
        // If it fails, it must be a DNS/resolution failure, not a security block.
        if let Err(e) = result {
            assert!(
                !e.contains("Blocked"),
                "Public URL should not be security-blocked: {e}"
            );
        }
    }
}
