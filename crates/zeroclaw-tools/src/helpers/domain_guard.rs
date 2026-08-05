//! Shared domain/URL validation and allowlist helpers.
//!
//! The primitives themselves live in [`zeroclaw_infra::net_guard`] so the tool
//! layer and the plugin host read one implementation without a tool-to-plugin
//! dependency (ADR-013). This module is the tool-layer facade: it keeps the
//! `crate::helpers::domain_guard::*` paths the tools and `zeroclaw-channels`
//! already use, and it is where a tool-specific or config-specific wrapper
//! would go if one were ever needed. Unit coverage for the primitives lives
//! beside them in `zeroclaw-infra`.

// ── allowlist normalization and matching ──────────────────────────
pub use zeroclaw_infra::net_guard::{
    host_matches_allowlist, normalize_allowed_domains, normalize_domain,
};

/// Check whether `host` is a private, loopback, link-local, or otherwise
/// non-globally-routable address (SSRF guard).
/// Handles both IPv4 and IPv6, as well as `localhost` and `.local` domains.
pub use zeroclaw_infra::net_guard::is_private_or_local_host;

// ── address classification and resolved-address validation ────────
// Crate-internal: these keep the visibility they had before the primitives
// moved to zeroclaw-infra, so the tool crate's public surface is unchanged.
pub(crate) use zeroclaw_infra::net_guard::{
    is_cloud_metadata_ip, is_non_global_v4, is_non_global_v6, validate_resolved_ips_are_public,
    validate_resolved_ips_exclude_metadata,
};
