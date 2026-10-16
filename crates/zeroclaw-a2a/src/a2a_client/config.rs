//! Client config — protocol-layer owned types, converted and supplied by the
//! host.
//!
//! The protocol crate does not depend on any config module: when configuration
//! is needed it is defined here, and `zeroclaw-tools` / `zeroclaw-gateway`
//! map their `zeroclaw-config` values into these types.

use std::sync::Arc;

use parking_lot::RwLock;

/// Single A2A peer, as seen by the outbound client.
///
/// Only outbound-relevant fields are present; server-publish fields
/// (`published` / `exposed_skills`) are not. `tags` is carried here so the
/// protocol layer's `a2a_discover` can `filter_tags` (the host passes the
/// operator tags through); it is not an A2A-REQUIRED field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct A2aPeerConfig {
    /// Peer name (the `peer` argument of every `a2a_*` tool).
    pub name: String,
    /// Peer base URL (the well-known card path and JSON-RPC path are derived
    /// from it).
    pub base_url: String,
    /// Bearer token; `${VAR}` is resolved by the host when converting; empty
    /// means anonymous.
    pub token: String,
    /// Optional operator tags for `a2a_discover` `filter_tags` (non-protocol).
    pub tags: Vec<String>,
}

/// Client config.
#[derive(Debug, Clone, Default)]
pub struct A2aClientConfig {
    /// Peer list.
    pub peers: Vec<A2aPeerConfig>,
    /// Agent Card cache TTL (seconds); `0` disables caching.
    pub card_cache_ttl_secs: u64,
    /// Response-body byte cap; `0` means unlimited.
    pub max_response_bytes: usize,
    /// Allowed NAT64 prefixes (SSRF validation).
    pub nat64_prefixes: Vec<String>,
    /// Whether private/loopback hosts are allowed (SSRF validation; default
    /// reject).
    pub allow_private_hosts: bool,
    /// Allowlisted private hosts (SSRF validation; only applies when
    /// `allow_private_hosts`).
    pub allowed_private_hosts: Vec<String>,
}

/// Hot-reload handle: the client reads the current configuration on each call.
pub type LiveA2aConfig = Arc<RwLock<A2aClientConfig>>;

/// Non-peer settings the client reads live on each call (card cache, response
/// cap, SSRF allowlists). Returned by [`A2aPeerSource::settings`].
#[derive(Debug, Clone, Default)]
pub struct A2aSettings {
    /// Agent Card cache TTL (seconds); `0` disables caching.
    pub card_cache_ttl_secs: u64,
    /// Response-body byte cap; `0` means unlimited.
    pub max_response_bytes: usize,
    /// Allowed NAT64 prefixes (SSRF validation).
    pub nat64_prefixes: Vec<String>,
    /// Whether private/loopback hosts are allowed (SSRF validation).
    pub allow_private_hosts: bool,
    /// Allowlisted private hosts (SSRF validation).
    pub allowed_private_hosts: Vec<String>,
}

/// Host-implemented source of outbound A2A state, read **live on every call**.
///
/// The client never owns a snapshot of peers, credentials, or policy: it holds
/// an `Arc<dyn A2aPeerSource>` and consults it per request, so a host config
/// reload (peer removed, token rotated, private-host tightening, proxy change)
/// takes effect on the next call — matching the pre-extraction behavior where
/// the client held the host's live config handle. Keeping this a trait (rather
/// than a concrete config type) preserves the protocol crate's boundary: it
/// depends on none of the host's config modules.
pub trait A2aPeerSource: Send + Sync {
    /// Current peers, with **raw** (undecrypted) token fields. Called per
    /// request; a duplicate or empty name is rejected by the client.
    fn peers(&self) -> Vec<A2aPeerConfig>;

    /// Decrypt a peer's raw token field to a plaintext literal. Host-owned
    /// (e.g. the canonical `SecretStore`); the client applies `${VAR}` env
    /// interpolation to the result. Only the resolved peer's token is passed
    /// here, so one peer's broken credential cannot disable the tool set.
    fn resolve_token(&self, raw: &str) -> anyhow::Result<String>;

    /// Live proxy policy for the A2A service, resolved from the host's runtime
    /// proxy configuration on each call.
    fn proxy_policy(&self) -> ProxyPolicy;

    /// Live non-peer settings (card cache, response cap, SSRF allowlists).
    fn settings(&self) -> A2aSettings;
}

/// `A2aPeerSource` backed by a [`LiveA2aConfig`] handle plus a fixed proxy
/// policy. Convenient for hosts/tests with a static proxy decision; peers and
/// settings are still read live from the handle on each call. Tokens are
/// treated as literals (no decryption) — use the host's own source for
/// encrypted credentials.
pub struct StaticA2aPeerSource {
    config: LiveA2aConfig,
    proxy: ProxyPolicy,
}

impl StaticA2aPeerSource {
    #[must_use]
    pub fn new(config: LiveA2aConfig, proxy: ProxyPolicy) -> Arc<Self> {
        Arc::new(Self { config, proxy })
    }
}

impl A2aPeerSource for StaticA2aPeerSource {
    fn peers(&self) -> Vec<A2aPeerConfig> {
        self.config.read().peers.clone()
    }

    fn resolve_token(&self, raw: &str) -> anyhow::Result<String> {
        Ok(raw.to_string())
    }

    fn proxy_policy(&self) -> ProxyPolicy {
        self.proxy.clone()
    }

    fn settings(&self) -> A2aSettings {
        let config = self.config.read();
        A2aSettings {
            card_cache_ttl_secs: config.card_cache_ttl_secs,
            max_response_bytes: config.max_response_bytes,
            nat64_prefixes: config.nat64_prefixes.clone(),
            allow_private_hosts: config.allow_private_hosts,
            allowed_private_hosts: config.allowed_private_hosts.clone(),
        }
    }
}

/// Proxy policy — the host maps its runtime proxy config into this enum.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyPolicy {
    /// No proxy (direct connection).
    #[default]
    Direct,
    /// Use the given proxy URL.
    Use(String),
}

/// Build a hot-reload config handle.
#[must_use]
pub fn live_config(config: A2aClientConfig) -> LiveA2aConfig {
    Arc::new(RwLock::new(config))
}
