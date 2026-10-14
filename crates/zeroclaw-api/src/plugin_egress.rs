//! Dependency-free value validation shared by plugin config and host egress.
//!
//! Host patterns are not here: they belong to `zeroclaw_infra::net_guard`,
//! which the built-in tools and the plugin egress authority both use.

/// Whether `name` is a valid TLS profile slug: 1 to 64 bytes of lowercase
/// ASCII letters, digits, `-`, or `_`, starting with a letter or digit.
///
/// Config validation and the plugin egress authority both call this, so the
/// two cannot disagree about which profile names exist.
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
    fn profile_names_are_lowercase_slugs() {
        for valid in ["corporate-mtls", "a", "private_ca_2", &"x".repeat(64)] {
            assert!(is_valid_tls_profile_name(valid), "{valid:?}");
        }
        for invalid in [
            "",
            "Corporate",
            "-leading",
            "has space",
            "dot.ted",
            &"x".repeat(65),
        ] {
            assert!(!is_valid_tls_profile_name(invalid), "{invalid:?}");
        }
    }
}
