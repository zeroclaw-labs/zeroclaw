//! GitHub provider for the git-forge channel.

mod api;
mod auth;
mod mapping;
mod payloads;
mod provider;

pub use provider::GithubProvider;

#[cfg(test)]
pub(crate) use auth::TEST_KEY_PEM;

#[cfg(test)]
pub(crate) mod api_test_support {
    /// Build a `GithubApi` whose base URL is a mock server.
    pub(crate) fn with_base(base: String) -> super::api::GithubApi {
        super::api::GithubApi::with_base(base)
    }
}
