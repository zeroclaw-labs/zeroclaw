//! GitHub provider for the git-forge channel.
//!
//! Layer: provider implementation. Implements
//! [`crate::git::traits::GitProvider`] for GitHub Apps, owning everything
//! forge-specific (auth, REST shapes, payload→event normalization,
//! endpoint quirks). Depends on the generic `git` contract; the generic
//! core never depends back on this module.
//!
//! - [`payloads`] — GitHub REST payload structs + provider-local constants.
//! - [`auth`] — private key + RS256 JWT + token cache (only key-touching file).
//! - [`api`] — typed REST wrappers; credentials passed in.
//! - [`mapping`] — payload→[`crate::git::events::GitEvent`] + reaction map.
//! - [`provider`] — composition root implementing `GitProvider`.

mod api;
mod auth;
mod mapping;
mod payloads;
mod provider;

pub use provider::GithubProvider;

#[cfg(test)]
pub(crate) use auth::TEST_KEY_PEM;

/// Test-only handles into the provider's internals, used by the generic
/// `GitChannel` mock-server suite to point the GitHub provider at a
/// wiremock server.
#[cfg(test)]
pub(crate) mod api_test_support {
    /// Build a `GithubApi` whose base URL is a mock server.
    pub(crate) fn with_base(base: String) -> super::api::GithubApi {
        super::api::GithubApi::with_base(base)
    }
}
