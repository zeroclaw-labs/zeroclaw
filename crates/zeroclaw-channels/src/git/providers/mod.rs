//! Git-forge provider implementations.
//!
//! Layer: provider wiring. Each forge is a sibling module implementing
//! [`crate::git::traits::GitProvider`], gated behind its own
//! `provider-<forge>` feature so a build can include only the forges it
//! needs. A new forge (e.g. GitLab) drops in here as `pub mod gitlab;`
//! with zero edits to the generic `git` core.

#[cfg(feature = "provider-gitea")]
pub mod gitea;
#[cfg(feature = "provider-github")]
pub mod github;

/// Map the provider-neutral [`ForgeMethod`](crate::git::types::ForgeMethod)
/// onto a `reqwest::Method`. Lives here so every provider shares one mapping
/// and the contract stays reqwest-free.
#[cfg(any(feature = "provider-github", feature = "provider-gitea"))]
pub(crate) fn forge_method_to_reqwest(method: crate::git::types::ForgeMethod) -> reqwest::Method {
    use crate::git::types::ForgeMethod;
    match method {
        ForgeMethod::Get => reqwest::Method::GET,
        ForgeMethod::Post => reqwest::Method::POST,
        ForgeMethod::Patch => reqwest::Method::PATCH,
        ForgeMethod::Put => reqwest::Method::PUT,
        ForgeMethod::Delete => reqwest::Method::DELETE,
    }
}
