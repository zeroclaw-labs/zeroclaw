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

/// Bound untrusted server-supplied text before it enters an error string,
/// a console line, or a log field: one owner for the limit so no path
/// forgets it. Truncation is char-boundary safe.
pub(crate) fn truncate_untrusted(text: &str) -> String {
    const LIMIT: usize = 256;
    if text.len() <= LIMIT {
        return text.to_string();
    }
    let mut end = LIMIT;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub mod browser;
pub mod challenge;
pub mod discovery;
pub mod listener;
pub mod manager;
pub mod metadata;
pub mod pkce;
pub mod policy;
pub mod refresh;
pub mod registration;
pub mod resource;
pub mod store;
pub mod tokens;
pub mod wiring;

pub use browser::{BrowserLauncher, SystemBrowserLauncher};
pub use challenge::{BearerChallenge, parse_bearer_challenge};
pub use discovery::{OAuthDiscovery, discover_oauth};
pub use listener::{CALLBACK_WAIT_TIMEOUT, CallbackFlow, LoopbackListener};
pub use manager::{McpOAuthManager, TransportOAuth};
pub use metadata::{AuthorizationServerMetadata, ProtectedResourceMetadata, ValidatedAs};
pub use pkce::Pkce;
pub use policy::{
    OAuthClientFactory, build_oauth_http_client, ensure_oauth_url, is_exact_loopback_host,
    origin_key,
};
pub use refresh::RefreshCoordinator;
pub use registration::{RegisteredClient, register_client};
pub use resource::CanonicalResource;
pub use store::{MCP_MODEL_PROVIDER, McpAuthorization, McpTokenStore, profile_name_for};
pub use tokens::{
    TokenEndpointError, TokenErrorKind, TokenSet, bearer_header_value, exchange_code,
    refresh_tokens,
};
pub use wiring::shared_oauth_manager;
