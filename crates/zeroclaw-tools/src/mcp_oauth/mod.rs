//! Generic OAuth for remote MCP HTTP/SSE transports.
//!
//! Scope (frozen in the Phase 0 design note): authorization-code grant with
//! mandatory PKCE (S256 only) for `transport = "http" | "sse"` servers.
//! RFC 9728 protected-resource metadata, RFC 8414 authorization-server
//! metadata, RFC 7591 dynamic registration of public clients, RFC 8707
//! `resource` binding, RFC 8252 loopback callback, and RFC 9207 `iss`
//! mix-up protection. No client secrets, no device flow, no mTLS/DPoP.
//!
//! The flow's only credential store is the encrypted auth-profile store
//! (see [`store`] once wired); OAuth token values never enter `config.toml`,
//! logs, errors, or `Debug` output.

pub mod challenge;
pub mod discovery;
pub mod metadata;
pub mod policy;
pub mod resource;

pub use challenge::{BearerChallenge, parse_bearer_challenge};
pub use discovery::{OAuthDiscovery, discover_oauth};
pub use metadata::{AuthorizationServerMetadata, ProtectedResourceMetadata, ValidatedAs};
pub use policy::{
    OAuthClientFactory, build_oauth_http_client, ensure_oauth_url, is_exact_loopback_host,
    origin_key, validate_advertised_message_endpoint,
};
pub use resource::CanonicalResource;
